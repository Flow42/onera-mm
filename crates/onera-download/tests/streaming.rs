//! Download tests against a mocked HTTP server.

use onera_core::hash::FileHash;
use onera_core::ports::{ArchiveStore, DownloadTarget};
use onera_core::progress::{CancelToken, NullProgress, ProgressEvent, RecordingProgress, Stage};
use onera_core::CoreError;
use onera_download::{ContentAddressedStore, DownloadConfig, Downloader};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    dir: tempfile::TempDir,
    store: Arc<ContentAddressedStore>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ContentAddressedStore::new(dir.path().join("archives")));
        Self { dir, store }
    }

    fn downloader(&self, config: DownloadConfig) -> Downloader {
        Downloader::new_for_tests(self.store.clone(), self.dir.path().join("tmp"), config).unwrap()
    }
}

fn target(server: &MockServer, path: &str, name: &str) -> DownloadTarget {
    DownloadTarget {
        url: url::Url::parse(&format!("{}{path}", server.uri())).unwrap(),
        headers: Vec::new(),
        expected_size: None,
        filename: name.to_owned(),
    }
}

#[tokio::test]
async fn downloads_stream_to_storage_and_are_hashed() {
    let server = MockServer::start().await;
    let payload = vec![b'x'; 100_000];
    Mock::given(method("GET"))
        .and(path("/file.zip"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .mount(&server)
        .await;

    let f = Fixture::new();
    let outcome = f
        .downloader(DownloadConfig::default())
        .fetch(
            &target(&server, "/file.zip", "file.zip"),
            None,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(outcome.bytes, 100_000);
    assert_eq!(outcome.hash, FileHash::blake3_of(&payload));
    assert!(!outcome.deduplicated);
    assert_eq!(outcome.path, f.store.path_for(&outcome.hash));
    assert_eq!(std::fs::read(&outcome.path).unwrap(), payload);
}

#[tokio::test]
async fn a_persisted_partial_download_resumes_with_a_byte_range() {
    let server = MockServer::start().await;
    let payload = b"already-downloaded-and-the-rest";
    let offset = 18;
    Mock::given(method("GET"))
        .and(path("/resume.zip"))
        .and(header("range", format!("bytes={offset}-")))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header(
                    "content-range",
                    format!("bytes {offset}-{}/{}", payload.len() - 1, payload.len()),
                )
                .set_body_bytes(payload[offset..].to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let f = Fixture::new();
    let partial = f.dir.path().join("persistent/job.part");
    std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
    std::fs::write(&partial, &payload[..offset]).unwrap();
    let mut download = target(&server, "/resume.zip", "resume.zip");
    download.expected_size = Some(payload.len() as u64);

    let outcome = f
        .downloader(DownloadConfig::default())
        .fetch_resumable(
            &download,
            None,
            &partial,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(outcome.bytes, payload.len() as u64);
    assert_eq!(outcome.hash, FileHash::blake3_of(payload));
    assert_eq!(std::fs::read(outcome.path).unwrap(), payload);
}

#[tokio::test]
async fn resume_restarts_safely_when_the_server_ignores_ranges() {
    let server = MockServer::start().await;
    let payload = b"complete archive";
    Mock::given(method("GET"))
        .and(path("/no-ranges.zip"))
        .and(header("range", "bytes=4-"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let f = Fixture::new();
    let partial = f.dir.path().join("persistent/job.part");
    std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
    std::fs::write(&partial, b"old!").unwrap();
    let mut download = target(&server, "/no-ranges.zip", "no-ranges.zip");
    download.expected_size = Some(payload.len() as u64);

    let outcome = f
        .downloader(DownloadConfig::default())
        .fetch_resumable(
            &download,
            None,
            &partial,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(std::fs::read(outcome.path).unwrap(), payload);
}

#[tokio::test]
async fn progress_is_reported_while_streaming() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 50_000]))
        .mount(&server)
        .await;

    let f = Fixture::new();
    let sink = RecordingProgress::default();
    f.downloader(DownloadConfig::default())
        .fetch(
            &target(&server, "/f", "f"),
            None,
            &sink,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    let events = sink.events();
    assert!(matches!(
        events.first(),
        Some(ProgressEvent::Started {
            stage: Stage::Downloading,
            ..
        })
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        ProgressEvent::Advanced {
            stage: Stage::Downloading,
            ..
        }
    )));
    assert!(matches!(
        events.last(),
        Some(ProgressEvent::Finished { success: true, .. })
    ));
}

#[tokio::test]
async fn an_already_stored_file_is_not_downloaded_again() {
    let server = MockServer::start().await;
    let payload = b"the same bytes".to_vec();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .expect(1) // exactly one request, not two
        .mount(&server)
        .await;

    let f = Fixture::new();
    let downloader = f.downloader(DownloadConfig::default());
    let first = downloader
        .fetch(
            &target(&server, "/f", "f"),
            None,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    let second = downloader
        .fetch(
            &target(&server, "/f", "f"),
            Some(&first.hash),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert!(second.deduplicated);
    assert_eq!(second.bytes, 0);
    assert_eq!(second.path, first.path);
}

#[tokio::test]
async fn a_hash_mismatch_is_rejected_and_the_file_is_discarded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"tampered payload".to_vec()))
        .mount(&server)
        .await;

    let f = Fixture::new();
    let expected = FileHash::blake3_of(b"what we asked for");
    let err = f
        .downloader(DownloadConfig::default())
        .fetch(
            &target(&server, "/f", "f"),
            Some(&expected),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, CoreError::IntegrityMismatch { .. }),
        "{err:?}"
    );
    assert!(!f.store.contains(&expected).await.unwrap());
}

#[tokio::test]
async fn a_truncated_response_is_detected_via_content_length() {
    let server = MockServer::start().await;
    // Declare more than is actually sent.
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "1000")
                .set_body_bytes(vec![b'x'; 10]),
        )
        .mount(&server)
        .await;

    let f = Fixture::new();
    let result = f
        .downloader(DownloadConfig {
            max_attempts: 1,
            ..DownloadConfig::default()
        })
        .fetch(
            &target(&server, "/f", "f"),
            None,
            &NullProgress,
            &CancelToken::new(),
        )
        .await;
    assert!(
        result.is_err(),
        "a short read must not be promoted into storage"
    );
}

#[tokio::test]
async fn an_oversized_download_is_refused_before_it_is_written() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 5_000]))
        .mount(&server)
        .await;

    let f = Fixture::new();
    let err = f
        .downloader(DownloadConfig {
            max_bytes: 100,
            max_attempts: 1,
            ..DownloadConfig::default()
        })
        .fetch(
            &target(&server, "/f", "f"),
            None,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("limit") || format!("{err}").contains("maximum"),
        "{err}"
    );
}

#[tokio::test]
async fn a_server_error_is_retried_and_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"eventually".to_vec()))
        .mount(&server)
        .await;

    let f = Fixture::new();
    let outcome = f
        .downloader(DownloadConfig::default())
        .fetch(
            &target(&server, "/f", "f"),
            None,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.bytes, 10);
}

#[tokio::test]
async fn a_client_error_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;

    let f = Fixture::new();
    let err = f
        .downloader(DownloadConfig::default())
        .fetch(
            &target(&server, "/f", "f"),
            None,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("403"), "{err}");
}

#[tokio::test]
async fn cancellation_stops_a_download_and_leaves_nothing_behind() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![b'x'; 5_000_000])
                .set_delay(Duration::from_millis(50)),
        )
        .mount(&server)
        .await;

    let f = Fixture::new();
    let cancel = CancelToken::new();
    cancel.cancel();

    let err = f
        .downloader(DownloadConfig::default())
        .fetch(&target(&server, "/f", "f"), None, &NullProgress, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");

    // No partial file should survive.
    let tmp = f.dir.path().join("tmp");
    let leftovers = std::fs::read_dir(&tmp).map(|d| d.count()).unwrap_or(0);
    assert_eq!(
        leftovers, 0,
        "a cancelled download left a partial file behind"
    );
}

#[tokio::test]
async fn cancelling_a_resumable_job_discards_its_partial_file() {
    let f = Fixture::new();
    let partial = f.dir.path().join("persistent/cancelled.part");
    std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
    std::fs::write(&partial, b"partial bytes").unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let cancelled_target = DownloadTarget {
        url: url::Url::parse("http://127.0.0.1:1/unused").unwrap(),
        headers: Vec::new(),
        expected_size: None,
        filename: "unused".into(),
    };

    let result = f
        .downloader(DownloadConfig::default())
        .fetch_resumable(&cancelled_target, None, &partial, &NullProgress, &cancel)
        .await;

    assert!(matches!(result, Err(CoreError::Cancelled)));
    assert!(!partial.exists());
}

#[tokio::test]
async fn concurrency_is_bounded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"payload".to_vec())
                .set_delay(Duration::from_millis(80)),
        )
        .mount(&server)
        .await;

    let f = Fixture::new();
    let downloader = Arc::new(f.downloader(DownloadConfig {
        max_concurrent: 2,
        ..DownloadConfig::default()
    }));

    let started = std::time::Instant::now();
    let mut handles = Vec::new();
    for i in 0..6 {
        let downloader = downloader.clone();
        let target = target(&server, &format!("/f{i}"), "f");
        handles.push(tokio::spawn(async move {
            downloader
                .fetch(&target, None, &NullProgress, &CancelToken::new())
                .await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    // Six 80 ms downloads two at a time cannot finish in under 200 ms.
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "downloads were not limited to two at a time ({:?})",
        started.elapsed()
    );
}
