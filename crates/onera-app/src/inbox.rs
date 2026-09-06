//! Running browser requests unattended.
//!
//! The browser extension queues a request and the desktop application runs it
//! without the user having to come back and click a second time. What "run"
//! means stops short of writing into a game, and deliberately so:
//!
//! | Request                | Runs to |
//! | ---------------------- | ------- |
//! | `add_mod`              | metadata cached; nothing transferred |
//! | `download`             | the archive in Onera's store |
//! | `download_and_install` | a previewed plan, applied only if nothing needs a decision |
//!
//! The last row is the whole rule. A plan whose files all land on untouched
//! paths asks the user nothing, so applying it silently is exactly what they
//! asked for by pressing "download and install". A plan that would overwrite
//! something unmanaged, something edited, or another mod's file is a question,
//! and it stops here with the preview kept for the window to show. Nothing the
//! user did not put there is overwritten without being asked, whichever side
//! the request came from.

use crate::flow::{InstallRequest, Onera, PreparedInstall};
use onera_core::ids::ProviderFileId;
use onera_core::progress::{CancelToken, ProgressSink};
use onera_core::{CoreError, Result};
use onera_db::jobs::{InboxRequest, InboxRequestKind};

/// What running one request achieved.
#[derive(Debug)]
pub enum RanRequest {
    /// Metadata was cached; the user still chooses what to do with it.
    Noted {
        /// Display name of the mod that was fetched.
        name: String,
    },
    /// The archive is in Onera's store.
    Downloaded {
        /// Display name of the mod.
        name: String,
        /// Bytes the stored archive occupies.
        bytes: u64,
    },
    /// The plan applied cleanly, because it needed no decision.
    Installed {
        /// Display name of the mod.
        name: String,
        /// Files written into the game.
        files: usize,
    },
    /// A plan is ready but needs the user. The caller keeps it for the window.
    NeedsDecision {
        /// Display name of the mod.
        name: String,
        /// The prepared plan, still unapplied.
        prepared: Box<PreparedInstall>,
    },
}

/// Run one queued browser request as far as it can go unattended.
///
/// The caller is expected to have taken the request's lease first — see
/// [`Onera::lease_inbox_request`] — so that a second watcher cannot start the
/// same transfer.
///
/// # Errors
/// Propagates provider, download, archive and planning errors. A failure here
/// is the caller's cue to record the request as failed, not to retry it: the
/// same request would fail the same way until something changes.
pub async fn run_request(
    onera: &Onera,
    request: &InboxRequest,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<RanRequest> {
    let details = onera
        .fetch_mod(&request.game_slug, &request.provider_mod_id, cancel)
        .await?;
    let name = details.name.clone();

    if request.kind == InboxRequestKind::AddMod {
        return Ok(RanRequest::Noted { name });
    }

    let file_id = request
        .provider_file_id
        .as_ref()
        .ok_or_else(|| CoreError::InvalidInput("the request names no file".to_owned()))?;
    let file = details
        .files
        .iter()
        .find(|candidate| candidate.provider_file_id == *file_id)
        .ok_or_else(|| CoreError::NotFound {
            kind: "provider file",
            id: file_id.as_str().to_owned(),
        })?
        .clone();

    if request.kind == InboxRequestKind::Download {
        let outcome = onera
            .download(
                &crate::flow::DownloadRequest {
                    game_slug: request.game_slug.clone(),
                    provider_mod_id: request.provider_mod_id.clone(),
                    provider_file_id: file.provider_file_id.clone(),
                    filename: file.name.clone(),
                    expected_size: file.size_bytes,
                    expected_hash: file.published_hash.clone(),
                },
                progress,
                cancel,
            )
            .await?;
        return Ok(RanRequest::Downloaded {
            name,
            bytes: outcome.bytes,
        });
    }

    let local_game_id = request.local_game_id.ok_or_else(|| {
        CoreError::InvalidInput("the request does not name a game to install into".to_owned())
    })?;
    let prepared = onera
        .prepare_install(
            &InstallRequest {
                local_game_id,
                game_slug: request.game_slug.clone(),
                mod_id: details.mod_id,
                release_id: file.release_id,
                provider_mod_id: request.provider_mod_id.clone(),
                provider_file_id: ProviderFileId::new(file.provider_file_id.as_str()),
                filename: file.name.clone(),
                expected_size: file.size_bytes,
                expected_hash: file.published_hash.clone(),
            },
            progress,
            cancel,
        )
        .await?;

    // The one place the unattended path is allowed to end in a write, and only
    // because a ready plan is one that asks nothing.
    if !prepared.plan.is_ready() {
        return Ok(RanRequest::NeedsDecision {
            name,
            prepared: Box::new(prepared),
        });
    }
    let report = onera.apply(&prepared, progress, cancel).await?;
    Ok(RanRequest::Installed {
        name,
        files: report.written,
    })
}
