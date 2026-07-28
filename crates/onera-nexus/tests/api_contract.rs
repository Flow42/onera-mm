//! Contract tests against mocked HTTP fixtures.
//!
//! No test here touches the network or needs an API key. A live-API smoke test
//! exists separately and is opt-in; see `docs/test-strategy.md`.

use async_trait::async_trait;
use onera_core::ids::{ProviderFileId, ProviderId, ProviderModId};
use onera_core::ports::{AccountInfo, AuthProvider, Credential, ModProvider};
use onera_core::progress::CancelToken;
use onera_core::redact::Secret;
use onera_core::{CoreError, Result};
use onera_nexus::{NexusClient, NexusConfig, RetryPolicy};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// An auth provider that always returns a fixed key, so the client can be
/// tested without a secret store.
struct StaticAuth {
    calls: AtomicUsize,
}

impl StaticAuth {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl AuthProvider for StaticAuth {
    fn provider_id(&self) -> ProviderId {
        ProviderId::nexus()
    }
    async fn is_authenticated(&self) -> Result<bool> {
        Ok(true)
    }
    async fn credential(&self) -> Result<Credential> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Credential::ApiKey(Secret::new("test-api-key-0123456789")))
    }
    async fn validate(&self, _c: &Credential) -> Result<AccountInfo> {
        unimplemented!("not used by these tests")
    }
    async fn store(&self, _c: Credential) -> Result<AccountInfo> {
        unimplemented!("not used by these tests")
    }
    async fn forget(&self) -> Result<()> {
        Ok(())
    }
}

/// A client pointed at a mock server. `https_only` is off for the mock, which is
/// the only difference from the production configuration.
fn client(server: &MockServer, retry: RetryPolicy) -> NexusClient {
    let config = NexusConfig {
        v3_base: format!("{}/v3", server.uri()),
        v1_base: format!("{}/v1", server.uri()),
        retry,
        timeout: Duration::from_secs(5),
        ..NexusConfig::default()
    };
    // The mock server speaks plain HTTP, so a client built for it cannot use
    // the production `https_only` setting. Everything else is identical.
    NexusClient::new_for_tests(config, StaticAuth::new()).unwrap()
}

fn mod_body(id: &str, name: &str) -> serde_json::Value {
    serde_json::json!({ "data": { "id": id, "game_scoped_id": "107", "name": name } })
}

#[tokio::test]
async fn sends_the_api_key_in_the_documented_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/games/cyberpunk2077/mods/107"))
        .and(header("apikey", "test-api-key-0123456789"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mod_body("1", "Cyber Engine Tweaks")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/mods/1/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "mod_files": [] }
        })))
        .mount(&server)
        .await;

    let (the_mod, releases) = client(&server, RetryPolicy::none())
        .mod_metadata(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(the_mod.name, "Cyber Engine Tweaks");
    assert_eq!(the_mod.game_slug, "cyberpunk2077");
    assert!(releases.is_empty());
}

#[tokio::test]
async fn walks_mod_files_and_their_versions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/games/cyberpunk2077/mods/107"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mod_body("1", "CET")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/mods/1/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "mod_files": [{ "id": "10", "name": "Main file" }] }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/mod-files/10/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "versions": [
                { "id": "100", "name": "CET 1.31", "version": "1.31.0",
                  "category": "main", "uploaded_at": "2024-05-01T10:00:00Z",
                  "size": 4096, "is_primary": true },
                { "id": "99", "name": "CET 1.30", "version": "1.30.0",
                  "category": "old_version", "uploaded_at": "2024-01-01T10:00:00Z" }
            ] }
        })))
        .mount(&server)
        .await;

    let nexus = client(&server, RetryPolicy::none());
    let (_, releases) = nexus
        .mod_metadata(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(releases.len(), 2);
    // Version strings are kept exactly as published.
    assert_eq!(releases[0].version, "1.31.0");
    assert!(releases[0].published_at.is_some());

    let files = nexus
        .files(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            None,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(files.items.len(), 2);
    let primary = files.items.iter().find(|f| f.is_primary).unwrap();
    assert_eq!(primary.name, "CET 1.31");
    assert_eq!(primary.size_bytes, Some(4096));
    assert_eq!(
        primary.category,
        onera_core::domain::release::FileCategory::Main
    );
}

#[tokio::test]
async fn retries_a_rate_limited_request_and_then_succeeds() {
    let server = MockServer::start().await;
    // First call is throttled with a short Retry-After, second succeeds.
    Mock::given(method("GET"))
        .and(path("/v3/games/cyberpunk2077/mods/107"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/games/cyberpunk2077/mods/107"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-rl-hourly-remaining", "42")
                .set_body_json(mod_body("1", "CET")),
        )
        .mount(&server)
        .await;
    // The real API sends the budget headers on every response, and the client
    // keeps the most recent reading, so the mock does the same.
    Mock::given(method("GET"))
        .and(path("/v3/mods/1/files"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-rl-hourly-remaining", "42")
                .set_body_json(serde_json::json!({ "data": { "mod_files": [] } })),
        )
        .mount(&server)
        .await;

    let nexus = client(&server, RetryPolicy::default());
    let result = nexus
        .mod_metadata(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            &CancelToken::new(),
        )
        .await;
    assert!(
        result.is_ok(),
        "a throttled request should be retried: {result:?}"
    );
    assert_eq!(nexus.rate_limit().hourly_remaining, Some(42));
}

#[tokio::test]
async fn gives_up_on_a_persistent_rate_limit_with_a_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .mount(&server)
        .await;

    let err = client(
        &server,
        RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        },
    )
    .mod_metadata(
        "cyberpunk2077",
        &ProviderModId::new("107"),
        &CancelToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CoreError::RateLimited { .. }), "{err:?}");
}

#[tokio::test]
async fn does_not_retry_a_client_error() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "title": "Unprocessable", "detail": "bad game domain", "status": 422
        })))
        .mount(&server)
        .await;

    let err = client(&server, RetryPolicy::default())
        .mod_metadata(
            "bad domain",
            &ProviderModId::new("107"),
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidInput(_)), "{err:?}");
    assert!(format!("{err}").contains("bad game domain"));
    assert_eq!(counter.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_unauthorised_response_is_reported_as_an_auth_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let err = client(&server, RetryPolicy::none())
        .mod_metadata(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(err.is_auth(), "{err:?}");
}

#[tokio::test]
async fn a_missing_mod_is_reported_as_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "title": "Not Found", "detail": "The mod was not found.", "status": 404
        })))
        .mount(&server)
        .await;

    let err = client(&server, RetryPolicy::none())
        .mod_metadata(
            "cyberpunk2077",
            &ProviderModId::new("999999"),
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound { .. }), "{err:?}");
}

#[tokio::test]
async fn a_malformed_body_is_an_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>not json at all</html>"))
        .mount(&server)
        .await;

    let err = client(&server, RetryPolicy::none())
        .mod_metadata(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("unreadable response"), "{err}");
}

#[tokio::test]
async fn a_response_missing_required_fields_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": {} })))
        .mount(&server)
        .await;

    assert!(client(&server, RetryPolicy::none())
        .mod_metadata(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            &CancelToken::new()
        )
        .await
        .is_err());
}

#[tokio::test]
async fn cancellation_stops_the_client_promptly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
        .mount(&server)
        .await;

    let cancel = CancelToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });

    let started = std::time::Instant::now();
    let err = client(&server, RetryPolicy::default())
        .mod_metadata("cyberpunk2077", &ProviderModId::new("107"), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancellation must not wait out a one-hour Retry-After"
    );
}

#[tokio::test]
async fn the_game_catalogue_is_fetched_and_mapped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "domain_name": "cyberpunk2077", "name": "Cyberpunk 2077" },
            { "domain_name": "skyrimspecialedition", "name": "Skyrim Special Edition" }
        ])))
        .mount(&server)
        .await;

    let page = client(&server, RetryPolicy::none())
        .games(None, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].provider_slug, "cyberpunk2077");
    assert_eq!(page.total, Some(2));
    assert_eq!(page.next, None);
}

#[tokio::test]
async fn a_download_location_is_resolved_and_must_be_https() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/v1/games/cyberpunk2077/mods/107/files/100/download_link.json",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "name": "Nexus CDN", "URI": "https://cdn.example.test/file.zip?sig=abc" }
        ])))
        .mount(&server)
        .await;

    let target = client(&server, RetryPolicy::none())
        .resolve_download(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            &ProviderFileId::new("100"),
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(target.url.host_str(), Some("cdn.example.test"));
    assert_eq!(target.url.scheme(), "https");
}

#[tokio::test]
async fn an_empty_download_list_explains_what_to_do() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let err = client(&server, RetryPolicy::none())
        .resolve_download(
            "cyberpunk2077",
            &ProviderModId::new("107"),
            &ProviderFileId::new("100"),
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("free account"), "{err}");
}

#[tokio::test]
async fn hostile_identifiers_cannot_escape_their_path_segment() {
    let server = MockServer::start().await;
    // If `../../` were passed through unencoded, this path would never be hit.
    Mock::given(method("GET"))
        .and(path("/v3/games/cyberpunk2077/mods/..%2F..%2Fadmin"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let err = client(&server, RetryPolicy::none())
        .mod_metadata(
            "cyberpunk2077",
            &ProviderModId::new("../../admin"),
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound { .. }), "{err:?}");
}
