//! Stopping a transfer the user no longer wants.
//!
//! Cancelling is not only about the bytes in flight. A job left in a resumable
//! state is picked up again by the next launch, which is precisely what a user
//! who pressed "cancel" did not ask for, and a partial file nothing will ever
//! resume is a file nothing will ever delete either. Both are what this file
//! pins down.

use async_trait::async_trait;
use onera_app::{Onera, Paths};
use onera_core::domain::game::Game;
use onera_core::ids::{DownloadJobId, ProviderFileId, ProviderId, ProviderModId};
use onera_core::ports::{
    AccountInfo, AuthProvider, Credential, DownloadGrant, DownloadTarget, ModProvider, Page,
};
use onera_core::progress::CancelToken;
use onera_core::CoreError;
use onera_download::{DownloadJob, JobState};
use std::sync::Arc;

struct NoProvider;

#[async_trait]
impl ModProvider for NoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::nexus()
    }
    async fn games(&self, _: Option<&str>, _: &CancelToken) -> onera_core::Result<Page<Game>> {
        Ok(Page::single(vec![]))
    }
    async fn mod_metadata(
        &self,
        _: &str,
        _: &ProviderModId,
        _: &CancelToken,
    ) -> onera_core::Result<(
        onera_core::domain::release::Mod,
        Vec<onera_core::domain::release::Release>,
    )> {
        Err(CoreError::Unsupported("no provider in this test".into()))
    }
    async fn files(
        &self,
        _: &str,
        _: &ProviderModId,
        _: Option<&str>,
        _: &CancelToken,
    ) -> onera_core::Result<Page<onera_core::domain::release::ProviderFile>> {
        Ok(Page::single(vec![]))
    }
    async fn resolve_download(
        &self,
        _: &str,
        _: &ProviderModId,
        _: &ProviderFileId,
        _: Option<&DownloadGrant>,
        _: &CancelToken,
    ) -> onera_core::Result<DownloadTarget> {
        Err(CoreError::Unsupported("no provider in this test".into()))
    }
}

struct NoAuth;

#[async_trait]
impl AuthProvider for NoAuth {
    fn provider_id(&self) -> ProviderId {
        ProviderId::nexus()
    }
    async fn is_authenticated(&self) -> onera_core::Result<bool> {
        Ok(false)
    }
    async fn credential(&self) -> onera_core::Result<Credential> {
        Err(CoreError::Unauthenticated {
            provider: "nexus".into(),
        })
    }
    async fn validate(&self, _: &Credential) -> onera_core::Result<AccountInfo> {
        Err(CoreError::Unauthenticated {
            provider: "nexus".into(),
        })
    }
    async fn store(&self, _: Credential) -> onera_core::Result<AccountInfo> {
        Err(CoreError::Unauthenticated {
            provider: "nexus".into(),
        })
    }
    async fn forget(&self) -> onera_core::Result<()> {
        Ok(())
    }
}

async fn onera(dir: &std::path::Path) -> Onera {
    Onera::assemble(
        Paths::rooted_at(dir.join("xdg")),
        Arc::new(NoAuth),
        Arc::new(NoProvider),
    )
    .await
    .unwrap()
}

/// A queued job with a partial file on disk, exactly as an interrupted
/// download leaves one.
async fn queued_job(onera: &Onera, dir: &std::path::Path) -> DownloadJob {
    let temp_path = dir.join("cet.zip.part");
    std::fs::write(&temp_path, b"half a download").unwrap();
    let mut job = DownloadJob::queued(
        ProviderId::nexus(),
        "cyberpunk2077".into(),
        ProviderModId::new("107"),
        ProviderFileId::new("9001"),
        "cet.zip".into(),
        Some(1_000),
        temp_path,
    );
    job.state = JobState::Paused;
    job.bytes_downloaded = 15;
    onera.database().put_download_job(&job).await.unwrap();
    job
}

#[tokio::test]
async fn cancelling_ends_a_job_nothing_will_resume_and_drops_its_partial() {
    let dir = tempfile::tempdir().unwrap();
    let onera = onera(dir.path()).await;
    let job = queued_job(&onera, dir.path()).await;

    onera.cancel_download(job.id).await.unwrap();

    let jobs = onera.downloads().await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, JobState::Cancelled);
    assert_eq!(jobs[0].bytes_downloaded, 0);
    assert!(
        !job.temp_path.exists(),
        "a cancelled job's partial file is never resumed, so it is not kept"
    );
    assert!(
        onera
            .database()
            .resumable_download_jobs()
            .await
            .unwrap()
            .is_empty(),
        "the next launch must not pick a cancelled download back up"
    );
}

#[tokio::test]
async fn cancelling_twice_is_not_an_error_and_an_unknown_job_is() {
    let dir = tempfile::tempdir().unwrap();
    let onera = onera(dir.path()).await;
    let job = queued_job(&onera, dir.path()).await;

    onera.cancel_download(job.id).await.unwrap();
    onera.cancel_download(job.id).await.unwrap();

    let error = onera
        .cancel_download(DownloadJobId::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error, CoreError::NotFound { kind, .. } if kind == "download job"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_finished_download_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let onera = onera(dir.path()).await;
    let mut job = queued_job(&onera, dir.path()).await;
    job.state = JobState::Complete;
    job.bytes_downloaded = 1_000;
    onera.database().put_download_job(&job).await.unwrap();

    // The archive is in the store; a cancel arriving late must not rewrite the
    // job into a state that says otherwise.
    onera.cancel_download(job.id).await.unwrap();

    let jobs = onera.downloads().await.unwrap();
    assert_eq!(jobs[0].state, JobState::Complete);
    assert_eq!(jobs[0].bytes_downloaded, 1_000);
}
