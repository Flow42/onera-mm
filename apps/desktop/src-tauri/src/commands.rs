//! Tauri commands.
//!
//! Each function does three things and nothing else: parse its arguments,
//! call one application method, and shape the result for the frontend. Any
//! decision more interesting than that belongs in [`onera_app`] or deeper.

use crate::state::{AppState, CommandError, CommandResult};
use onera_core::domain::baseline::BaselineSource;
use onera_core::ids::{InstallationId, LocalGameId, OperationId, ProviderFileId, ProviderModId};
use onera_core::plan::{ConflictChoice, Decision, DecisionScope, InstallPlan, TargetLocation};
use onera_core::progress::NullProgress;
use onera_core::redact::Secret;
use onera_core::RelPath;
use onera_install::remove::ModifiedFilePolicy;
use serde_json::json;
use std::str::FromStr;
use tauri::State;
use tauri_plugin_dialog::DialogExt as _;
use tauri_plugin_opener::OpenerExt as _;

fn parse_game(id: &str) -> CommandResult<LocalGameId> {
    LocalGameId::from_str(id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid game id".into(),
    })
}

fn parse_installation(id: &str) -> CommandResult<InstallationId> {
    InstallationId::from_str(id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid installation id".into(),
    })
}

fn parse_operation(id: &str) -> CommandResult<OperationId> {
    OperationId::from_str(id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid operation id".into(),
    })
}

// ---------------------------------------------------------------------------
// Onboarding
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn startup_status(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    let recovery_required = !state.onera.interrupted_operations().await?.is_empty();
    // Activation records left behind by a dead process are finished here, after
    // journal recovery has had its say. None of them can make a target profile
    // active: only the completion transaction does that.
    let finalized_activations = state.onera.recover_profile_activations().await?.len();
    Ok(json!({
        "authenticated": state.onera.is_authenticated().await?,
        "recovery_required": recovery_required,
        "finalized_activations": finalized_activations,
        "inbox_count": state.onera.inbox_requests().await?.len(),
        "expired_plans": state.onera.expired_prepared_plans(),
    }))
}

#[tauri::command]
pub async fn is_authenticated(state: State<'_, AppState>) -> CommandResult<bool> {
    Ok(state.onera.is_authenticated().await?)
}

/// Validate and store a personal API key.
///
/// The key arrives as a plain string from the frontend, is immediately wrapped
/// in a [`Secret`] and is never returned, logged or echoed.
#[tauri::command]
pub async fn set_api_key(
    state: State<'_, AppState>,
    key: String,
) -> CommandResult<serde_json::Value> {
    let account = state.onera.set_api_key(Secret::new(key)).await?;
    Ok(json!({
        "provider_user_id": account.provider_user_id,
        "username": account.username,
        "premium": account.premium,
        "email": account.email,
    }))
}

#[tauri::command]
pub async fn forget_api_key(state: State<'_, AppState>) -> CommandResult<()> {
    Ok(state.onera.forget_api_key().await?)
}

#[tauri::command]
pub async fn account(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    let account = state.onera.account().await?;
    Ok(json!({
        "provider_user_id": account.provider_user_id,
        "username": account.username,
        "premium": account.premium,
        "email": account.email,
    }))
}

// ---------------------------------------------------------------------------
// Games
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn discover_games(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let found = state.onera.discover_games(&cancel).await?;
    // Serialize the domain type itself rather than rebuilding the shape by
    // hand: the frontend hands the same value straight back to `confirm_game`,
    // so the two sides have to agree field for field, `source` included.
    Ok(serde_json::to_value(&found).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
pub async fn confirm_game(
    state: State<'_, AppState>,
    game: serde_json::Value,
) -> CommandResult<String> {
    let discovered: onera_discovery::DiscoveredGame =
        serde_json::from_value(game).map_err(|e| CommandError {
            code: "internal".into(),
            message: format!("could not read the game description: {e}"),
        })?;
    Ok(state.onera.confirm_game(&discovered).await?.to_string())
}

#[tauri::command]
pub async fn add_manual_game(path: String) -> CommandResult<serde_json::Value> {
    let adapters = onera_games::all_adapters();
    let found = onera_discovery::add_manual(std::path::Path::new(&path), &adapters)?;
    Ok(serde_json::to_value(&found).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
pub async fn local_games(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    let games = state.onera.local_games().await?;
    Ok(json!(games
        .iter()
        .map(|g| json!({
            "id": g.id.to_string(),
            "adapter_id": g.adapter_id,
            "install_root": g.install_root,
            "confirmed": g.confirmed,
        }))
        .collect::<Vec<_>>()))
}

/// Where one game is installed and where its archives are extracted.
///
/// Both are directories the user may want to look at, and the second is one
/// they may want to change: a game on another disk is better staged on that
/// disk than copied across a filesystem boundary twice per install.
#[tauri::command]
pub async fn game_paths(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    let game = parse_game(&game_id)?;
    let install = state
        .onera
        .local_games()
        .await?
        .into_iter()
        .find(|candidate| candidate.id == game)
        .ok_or_else(|| CommandError {
            code: "not_found".into(),
            message: "that game is not registered".into(),
        })?;
    let staging = state.onera.staging_info(game).await?;
    Ok(json!({
        "install_root": install.install_root,
        "staging_root": staging.root,
        "staging_is_default": staging.is_default,
        "staging_entries": staging.entries,
    }))
}

/// Point one game's extractions at another directory.
///
/// The directory has to be empty, and Onera says why rather than silently
/// choosing something else: everything under a staging root is deleted when
/// Onera starts, so it will only take one that has nothing to lose. Whatever is
/// in the old root — an unfinished extraction — moves across with the setting.
#[tauri::command]
pub async fn set_game_staging_root(
    state: State<'_, AppState>,
    game_id: String,
    path: String,
) -> CommandResult<serde_json::Value> {
    let change = state
        .onera
        .set_staging_root(parse_game(&game_id)?, std::path::Path::new(&path))
        .await?;
    Ok(serde_json::to_value(change).unwrap_or(serde_json::Value::Null))
}

/// Ask the user for a staging directory, and use it if they choose one.
///
/// The picker runs here rather than in the window because the whole change is
/// one decision: choose a directory, have it checked, have the unfinished work
/// in the old one moved across. Splitting it would leave a window able to point
/// staging at a path the user never saw.
///
/// `None` means the user cancelled, which is not an error.
#[tauri::command]
pub async fn pick_game_staging_root(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<Option<serde_json::Value>> {
    let game = parse_game(&game_id)?;
    let current = state.onera.staging_info(game).await?;

    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose an empty directory for staging")
        .set_directory(&current.root)
        .pick_folder(move |chosen| {
            // The receiver is only dropped if the window went away first, in
            // which case there is nothing to tell.
            let _ = send.send(chosen);
        });
    let Ok(Some(chosen)) = receive.await else {
        return Ok(None);
    };
    let path = chosen.into_path().map_err(|e| CommandError {
        code: "internal".into(),
        message: format!("that directory could not be read: {e}"),
    })?;

    let change = state.onera.set_staging_root(game, &path).await?;
    Ok(Some(
        serde_json::to_value(change).unwrap_or(serde_json::Value::Null),
    ))
}

/// Return one game to Onera's own staging directory.
#[tauri::command]
pub async fn reset_game_staging_root(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    let change = state.onera.reset_staging_root(parse_game(&game_id)?).await?;
    Ok(serde_json::to_value(change).unwrap_or(serde_json::Value::Null))
}

/// Where downloads are kept, for one game or for everything.
///
/// `gameId` is optional on purpose: the settings screen asks about the shared
/// directory, and a game's page asks about that game — which answers with the
/// shared directory too when the game has not overridden it, and says so.
#[tauri::command]
pub async fn download_paths(
    state: State<'_, AppState>,
    game_id: Option<String>,
) -> CommandResult<serde_json::Value> {
    let game = game_id.as_deref().map(parse_game).transpose()?;
    let info = state.onera.download_dir_info(game).await?;
    Ok(serde_json::to_value(info).unwrap_or(serde_json::Value::Null))
}

/// Ask the user where downloads should go, and move what is there already.
///
/// With a `gameId` this sets that game's own directory, overriding the shared
/// one; without, it sets the shared one. `None` means the user cancelled.
#[tauri::command]
pub async fn pick_download_root(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    game_id: Option<String>,
) -> CommandResult<Option<serde_json::Value>> {
    let game = game_id.as_deref().map(parse_game).transpose()?;
    let current = state.onera.download_dir_info(game).await?;

    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose where downloaded mods are kept")
        .set_directory(&current.root)
        .pick_folder(move |chosen| {
            let _ = send.send(chosen);
        });
    let Ok(Some(chosen)) = receive.await else {
        return Ok(None);
    };
    let path = chosen.into_path().map_err(|e| CommandError {
        code: "internal".into(),
        message: format!("that directory could not be read: {e}"),
    })?;

    let change = match game {
        Some(game) => state.onera.set_game_download_root(game, &path).await?,
        None => state.onera.set_download_root(&path).await?,
    };
    Ok(Some(
        serde_json::to_value(change).unwrap_or(serde_json::Value::Null),
    ))
}

/// Drop an override: a game returns to the shared directory, and the shared
/// directory returns to Onera's own.
#[tauri::command]
pub async fn reset_download_root(
    state: State<'_, AppState>,
    game_id: Option<String>,
) -> CommandResult<serde_json::Value> {
    let change = match game_id.as_deref().map(parse_game).transpose()? {
        Some(game) => state.onera.reset_game_download_root(game).await?,
        None => state.onera.reset_download_root().await?,
    };
    Ok(serde_json::to_value(change).unwrap_or(serde_json::Value::Null))
}

/// A game's own icon, found in its installation directory, as a data URI.
///
/// `None` is an ordinary answer: plenty of games ship no image, and the card
/// falls back to the initials it already draws.
#[tauri::command]
pub async fn game_icon(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<Option<String>> {
    let Some(icon) = state.onera.game_icon(parse_game(&game_id)?).await? else {
        return Ok(None);
    };
    Ok(Some(format!(
        "data:{};base64,{}",
        icon.content_type,
        base64(&icon.bytes)
    )))
}

/// Open the provider's mod listing for a game in the user's browser.
///
/// Mods are added from the browser extension, so the desktop's only part in
/// finding one is sending the user to the right page.
#[tauri::command]
pub fn open_mod_page(app: tauri::AppHandle, adapter_id: String) -> CommandResult<()> {
    let adapter = onera_games::adapter_by_id(&adapter_id).ok_or_else(|| CommandError {
        code: "internal".into(),
        message: format!("no adapter named {adapter_id:?}"),
    })?;
    let slug = adapter
        .provider_slugs()
        .first()
        .ok_or_else(|| CommandError {
            code: "internal".into(),
            message: "that game has no page on the provider".into(),
        })?;
    app.opener()
        .open_url(
            format!("https://www.nexusmods.com/{slug}/mods"),
            None::<&str>,
        )
        .map_err(|e| CommandError {
            code: "internal".into(),
            message: format!("the browser could not be opened: {e}"),
        })
}

// ---------------------------------------------------------------------------
// Mods
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn fetch_mod(
    state: State<'_, AppState>,
    game_domain: String,
    mod_id: String,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let details = state
        .onera
        .fetch_mod(&game_domain, &ProviderModId::new(mod_id), &cancel)
        .await?;
    Ok(json!({
        "mod_id": details.mod_id.to_string(),
        "name": details.name,
        "author": details.author,
        "needs_file_selection": details.needs_file_selection(),
        "files": details.files.iter().map(|f| json!({
            "id": f.provider_file_id.as_str(),
            "name": f.name,
            "category": format!("{:?}", f.category).to_lowercase(),
            "size": f.size_bytes,
            "is_primary": f.is_primary,
        })).collect::<Vec<_>>(),
    }))
}

#[tauri::command]
pub async fn installed_mods(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    Ok(
        serde_json::to_value(state.onera.installed_mods(parse_game(&game_id)?).await?)
            .unwrap_or(serde_json::Value::Null),
    )
}

#[tauri::command]
pub async fn check_updates(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    Ok(serde_json::to_value(
        state
            .onera
            .check_updates(parse_game(&game_id)?, &cancel)
            .await?,
    )
    .unwrap_or(serde_json::Value::Null))
}

/// Open one mod's own page on the provider.
///
/// The address is rebuilt from the identifiers Onera stored rather than from a
/// URL handed in by a caller: a stored string that reaches the system's URL
/// handler is a string worth not trusting, and the provider's page shape is
/// already known here.
#[tauri::command]
pub fn open_nexus_mod(
    app: tauri::AppHandle,
    game_slug: String,
    provider_mod_id: String,
) -> CommandResult<()> {
    let safe = |value: &str| {
        !value.is_empty()
            && value.len() <= 64
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    if !safe(&game_slug) || !safe(&provider_mod_id) {
        return Err(CommandError {
            code: "internal".into(),
            message: "that mod has no page address Onera can open".into(),
        });
    }
    app.opener()
        .open_url(
            format!("https://www.nexusmods.com/{game_slug}/mods/{provider_mod_id}"),
            None::<&str>,
        )
        .map_err(|e| CommandError {
            code: "internal".into(),
            message: format!("the browser could not be opened: {e}"),
        })
}

/// One mod's artwork, as a data URI the view can put straight into an `img`.
///
/// A data URI rather than a URL because the window makes no requests of its
/// own: the image is fetched and cached by the core, and the frontend's content
/// security policy needs no host added to it. `null` means the mod has no
/// artwork, or that it could not be fetched — either way the list draws.
#[tauri::command]
pub async fn mod_artwork(
    state: State<'_, AppState>,
    mod_id: String,
) -> CommandResult<Option<String>> {
    let id = onera_core::ids::ModId::from_str(&mod_id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid mod id".into(),
    })?;
    let cancel = onera_core::progress::CancelToken::new();
    let Some(artwork) = state.onera.mod_artwork(id, &cancel).await? else {
        return Ok(None);
    };
    Ok(Some(format!(
        "data:{};base64,{}",
        artwork.content_type,
        base64(&artwork.bytes)
    )))
}

/// The files one installed mod owns, and the directory that holds them all.
///
/// `limit` bounds the list, never the count: the view shows the first few and
/// says how many there are.
#[tauri::command]
pub async fn mod_contents(
    state: State<'_, AppState>,
    game_id: String,
    installation_id: String,
    limit: usize,
) -> CommandResult<serde_json::Value> {
    let contents = state
        .onera
        .mod_contents(
            parse_game(&game_id)?,
            parse_installation(&installation_id)?,
            limit.min(500),
        )
        .await?;
    Ok(serde_json::to_value(contents).unwrap_or(serde_json::Value::Null))
}

/// Show one mod's files in the system file manager.
///
/// The directory is recomputed here from what Onera deployed rather than
/// accepted from the window: a command that opened a path it was handed would
/// be a command that opens anything at all.
#[tauri::command]
pub async fn browse_mod_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    installation_id: String,
) -> CommandResult<String> {
    let path = state
        .onera
        .mod_browse_path(parse_game(&game_id)?, parse_installation(&installation_id)?)
        .await?;
    app.opener()
        .open_path(path.display().to_string(), None::<&str>)
        .map_err(|e| CommandError {
            code: "internal".into(),
            message: format!("the file manager could not be opened: {e}"),
        })?;
    Ok(path.display().to_string())
}

/// Encode bytes as standard base64.
///
/// Written out rather than pulled in: one call site, no padding subtleties, and
/// a dependency fewer in the crate that renders the window.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[tauri::command]
pub async fn inbox_requests(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    Ok(
        serde_json::to_value(state.onera.inbox_requests().await?)
            .unwrap_or(serde_json::Value::Null),
    )
}

#[tauri::command]
pub async fn dismiss_inbox_request(
    state: State<'_, AppState>,
    request_id: String,
) -> CommandResult<()> {
    let id = request_id
        .parse::<uuid::Uuid>()
        .map(onera_core::ids::InboxRequestId::from)
        .map_err(|_| CommandError {
            code: "invalid_input".into(),
            message: "that is not a valid inbox request id".into(),
        })?;
    state.onera.dismiss_inbox_request(id).await?;
    Ok(())
}

#[tauri::command]
pub async fn complete_inbox_request(
    state: State<'_, AppState>,
    request_id: String,
) -> CommandResult<()> {
    let id = request_id
        .parse::<uuid::Uuid>()
        .map(onera_core::ids::InboxRequestId::from)
        .map_err(|_| CommandError {
            code: "invalid_input".into(),
            message: "that is not a valid inbox request id".into(),
        })?;
    state.onera.complete_inbox_request(id).await?;
    Ok(())
}

/// Stop a queued browser request, and whatever it had already started.
///
/// The request leaves the inbox either way. Anything it downloaded before the
/// cancel stays in Onera's store — a partial transfer is discarded, but bytes
/// that already became an archive are not thrown away for a cancel.
#[tauri::command]
pub async fn cancel_inbox_request(
    state: State<'_, AppState>,
    request_id: String,
) -> CommandResult<()> {
    let id = request_id
        .parse::<uuid::Uuid>()
        .map(onera_core::ids::InboxRequestId::from)
        .map_err(|_| CommandError {
            code: "invalid_input".into(),
            message: "that is not a valid inbox request id".into(),
        })?;
    // Told to stop first, then dismissed: the watcher records the outcome of
    // the request it was running, and a dismissal written after that is the one
    // the user sees.
    if let Some(token) = state.inbox_cancels.lock().await.get(&id) {
        token.cancel();
    }
    state.onera.dismiss_inbox_request(id).await?;
    Ok(())
}

#[tauri::command]
pub async fn downloads(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    Ok(serde_json::to_value(state.onera.downloads().await?).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
pub async fn download_file(
    state: State<'_, AppState>,
    game_domain: String,
    mod_id: String,
    file_id: String,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let details = state
        .onera
        .fetch_mod(&game_domain, &ProviderModId::new(&mod_id), &cancel)
        .await?;
    let file = details
        .files
        .iter()
        .find(|candidate| candidate.provider_file_id.as_str() == file_id)
        .ok_or_else(|| CommandError {
            code: "not_found".into(),
            message: "that file is not offered by this mod".into(),
        })?;
    let outcome = state
        .onera
        .download(
            &onera_app::DownloadRequest {
                game_slug: game_domain,
                provider_mod_id: ProviderModId::new(mod_id),
                provider_file_id: file.provider_file_id.clone(),
                filename: file.name.clone(),
                expected_size: file.size_bytes,
                expected_hash: file.published_hash.clone(),
                // A download started from the window has no browser click
                // behind it, so nothing authorised it but the API key.
                grant: None,
            },
            &state.progress(),
            &cancel,
        )
        .await?;
    Ok(json!({
        "archive_id": outcome.archive_id.to_string(),
        "hash": outcome.hash.to_string(),
        "bytes": outcome.bytes,
        "deduplicated": outcome.deduplicated,
    }))
}

/// Stop a download the user no longer wants.
///
/// Cancellation is cooperative: a transfer stops at its next safe point, and
/// the job is left in a state nothing resumes.
#[tauri::command]
pub async fn cancel_download(state: State<'_, AppState>, job_id: String) -> CommandResult<()> {
    let id = job_id
        .parse::<uuid::Uuid>()
        .map(onera_core::ids::DownloadJobId::from)
        .map_err(|_| CommandError {
            code: "invalid_input".into(),
            message: "that is not a valid download job id".into(),
        })?;
    state.onera.cancel_download(id).await?;
    Ok(())
}

#[tauri::command]
pub async fn resume_downloads(state: State<'_, AppState>) -> CommandResult<()> {
    state
        .onera
        .resume_downloads(&state.progress(), &onera_core::progress::CancelToken::new())
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

/// Serialise a plan for the preview view.
fn plan_view(
    plan: &InstallPlan,
    prepared: &onera_app::PreparedInstall,
    name: &str,
) -> serde_json::Value {
    json!({
        "operation_id": plan.operation_id.to_string(),
        "installation_id": plan.installation_id.to_string(),
        "mod_name": name,
        "layout_rationale": prepared.layout_rationale,
        "ignored": prepared.ignored,
        "rejected": prepared.rejected_entries.iter().map(|r| json!({
            "raw_path": r.raw_path, "reason": r.reason,
        })).collect::<Vec<_>>(),
        "ready": plan.is_ready(),
        "bytes_to_write": plan.bytes_to_write(),
        "files": plan.files.iter().map(|f| json!({
            "source": f.source.as_str(),
            "target": f.target.to_string(),
            "classification": f.classification,
            "action": f.effective_action(),
            "existing_hash": f.existing_hash.as_ref().map(|h| h.to_string()),
            "notes": f.notes,
            "decision": f.decision,
        })).collect::<Vec<_>>(),
    })
}

#[tauri::command]
pub async fn prepare_install(
    state: State<'_, AppState>,
    game_id: String,
    game_domain: String,
    mod_id: String,
    file_id: String,
) -> CommandResult<serde_json::Value> {
    let game = parse_game(&game_id)?;
    let cancel = onera_core::progress::CancelToken::new();
    let details = state
        .onera
        .fetch_mod(&game_domain, &ProviderModId::new(&mod_id), &cancel)
        .await?;
    let file = details
        .files
        .iter()
        .find(|f| f.provider_file_id.as_str() == file_id)
        .ok_or_else(|| CommandError {
            code: "not_found".into(),
            message: "that file is not offered by this mod".into(),
        })?;

    let progress = state.progress();
    let prepared = state
        .onera
        .prepare_install(
            &onera_app::InstallRequest {
                local_game_id: game,
                game_slug: game_domain,
                mod_id: details.mod_id,
                release_id: file.release_id,
                provider_mod_id: ProviderModId::new(mod_id),
                provider_file_id: ProviderFileId::new(file_id),
                filename: file.name.clone(),
                expected_size: file.size_bytes,
                expected_hash: file.published_hash.clone(),
                grant: None,
            },
            &progress,
            &cancel,
        )
        .await?;

    let view = plan_view(&prepared.plan, &prepared, &details.name);
    state
        .cancels
        .lock()
        .await
        .insert(prepared.plan.operation_id, cancel);
    state
        .prepared
        .lock()
        .await
        .insert(prepared.plan.operation_id, prepared);
    Ok(view)
}

#[tauri::command]
pub async fn decide(
    state: State<'_, AppState>,
    operation_id: String,
    target: String,
    choice: String,
    scope: String,
) -> CommandResult<serde_json::Value> {
    let operation = parse_operation(&operation_id)?;
    let choice = match choice.as_str() {
        "keep_existing" => ConflictChoice::KeepExisting,
        "replace_after_backup" => ConflictChoice::ReplaceAfterBackup,
        "adopt_existing" => ConflictChoice::AdoptExisting,
        "abort" => ConflictChoice::Abort,
        other => {
            return Err(CommandError {
                code: "internal".into(),
                message: format!("unknown conflict choice {other:?}"),
            })
        }
    };

    let mut prepared_map = state.prepared.lock().await;
    let prepared = prepared_map
        .get_mut(&operation)
        .ok_or_else(|| CommandError {
            code: "not_found".into(),
            message: "that installation preview has expired".into(),
        })?;

    let (root_key, path) = target.split_once(':').ok_or_else(|| CommandError {
        code: "internal".into(),
        message: "malformed target".into(),
    })?;
    let location = TargetLocation {
        root_key: root_key.to_owned(),
        path: RelPath::normalize(path)?,
    };

    let classification = prepared
        .plan
        .files
        .iter()
        .find(|f| f.target == location)
        .map(|f| f.classification)
        .ok_or_else(|| CommandError {
            code: "not_found".into(),
            message: "that file is not part of this plan".into(),
        })?;

    let scope = match scope.as_str() {
        "equivalent_in_operation" => DecisionScope::EquivalentInThisOperation { classification },
        "remembered_rule" => DecisionScope::RememberedRule {
            mod_id: prepared.plan.mod_id,
            root_key: location.root_key.clone(),
            // A remembered rule is scoped to the containing directory, never to
            // the whole game: a broad rule would silently overwrite files the
            // user never considered.
            path_prefix: location
                .path
                .parent()
                .map(|p| format!("{p}/"))
                .unwrap_or_default(),
        },
        _ => DecisionScope::ThisFile,
    };

    prepared
        .plan
        .apply_decision(&location, &Decision { choice, scope });
    Ok(plan_view(&prepared.plan, prepared, ""))
}

#[tauri::command]
pub async fn apply_plan(
    state: State<'_, AppState>,
    operation_id: String,
) -> CommandResult<serde_json::Value> {
    let operation = parse_operation(&operation_id)?;
    let prepared = state
        .prepared
        .lock()
        .await
        .remove(&operation)
        .ok_or_else(|| CommandError {
            code: "not_found".into(),
            message: "that installation preview has expired".into(),
        })?;

    let cancel = state.cancel_token(operation).await;
    let progress = state.progress();
    let report = state.onera.apply(&prepared, &progress, &cancel).await?;
    state.cancels.lock().await.remove(&operation);
    Ok(json!({
        "written": report.written,
        "shared": report.shared,
        "skipped": report.skipped,
        "backed_up": report.backed_up,
        "installation_id": prepared.plan.installation_id.to_string(),
    }))
}

/// Request cancellation of an in-flight operation.
///
/// Cancellation is cooperative: the core stops at its next safe point, so an
/// operation that has begun renaming files finishes those renames rather than
/// leaving a game half-written.
#[tauri::command]
pub async fn cancel_operation(
    state: State<'_, AppState>,
    operation_id: String,
) -> CommandResult<()> {
    let operation = parse_operation(&operation_id)?;
    if let Some(token) = state.cancels.lock().await.get(&operation) {
        token.cancel();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Verify, remove, history, recovery
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn verify(
    state: State<'_, AppState>,
    game_id: String,
    installation_id: String,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let progress = state.progress();
    let report = state
        .onera
        .verify(
            parse_game(&game_id)?,
            parse_installation(&installation_id)?,
            &progress,
            &cancel,
        )
        .await?;
    Ok(serde_json::to_value(&report).unwrap_or(serde_json::Value::Null))
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

fn parse_profile(id: &str) -> CommandResult<onera_core::ids::ProfileId> {
    onera_core::ids::ProfileId::from_str(id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid profile id".into(),
    })
}

/// Every profile for a game. Exactly one is active.
#[tauri::command]
pub async fn profiles(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    let profiles = state.onera.profiles(parse_game(&game_id)?).await?;
    Ok(serde_json::to_value(&profiles).unwrap_or(serde_json::Value::Null))
}

/// One profile's members, lowest priority first.
///
/// `installation_id: null` is a member whose artifact is not downloaded yet —
/// a download in the activation preview, never an omission.
#[tauri::command]
pub async fn profile_members(
    state: State<'_, AppState>,
    profile_id: String,
) -> CommandResult<serde_json::Value> {
    let details = state
        .onera
        .profile_details(parse_profile(&profile_id)?)
        .await?;
    Ok(serde_json::to_value(&details.members).unwrap_or(serde_json::Value::Null))
}

fn parse_member(id: &str) -> CommandResult<onera_core::ids::ProfileMemberId> {
    onera_core::ids::ProfileMemberId::from_str(id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid profile member id".into(),
    })
}

/// Create an empty profile, or duplicate one as a starting point.
#[tauri::command]
pub async fn create_profile(
    state: State<'_, AppState>,
    game_id: String,
    name: String,
    description: Option<String>,
    copy_from_profile_id: Option<String>,
) -> CommandResult<serde_json::Value> {
    let copy_from = copy_from_profile_id
        .as_deref()
        .map(parse_profile)
        .transpose()?;
    let profile = state
        .onera
        .create_profile(parse_game(&game_id)?, name, description, copy_from)
        .await?;
    Ok(serde_json::to_value(&profile).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
pub async fn rename_profile(
    state: State<'_, AppState>,
    profile_id: String,
    name: String,
) -> CommandResult<serde_json::Value> {
    let profile = state
        .onera
        .rename_profile(parse_profile(&profile_id)?, name)
        .await?;
    Ok(serde_json::to_value(&profile).unwrap_or(serde_json::Value::Null))
}

/// Delete an inactive profile.
///
/// Deleting the active one returns `conflict`: another profile must be
/// activated first, so a game is never left without one.
#[tauri::command]
pub async fn delete_profile(state: State<'_, AppState>, profile_id: String) -> CommandResult<()> {
    state
        .onera
        .delete_profile(parse_profile(&profile_id)?)
        .await?;
    Ok(())
}

/// Add a mod lineage to a profile's desired state. Changes nothing on disk.
#[tauri::command]
pub async fn add_profile_member(
    state: State<'_, AppState>,
    profile_id: String,
    mod_id: String,
    provider_file_id: Option<String>,
) -> CommandResult<serde_json::Value> {
    let mod_id = onera_core::ids::ModId::from_str(&mod_id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid mod id".into(),
    })?;
    let member = state
        .onera
        .add_profile_member(
            parse_profile(&profile_id)?,
            mod_id,
            provider_file_id.map(ProviderFileId::new),
        )
        .await?;
    Ok(serde_json::to_value(&member).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
pub async fn remove_profile_member(
    state: State<'_, AppState>,
    member_id: String,
) -> CommandResult<()> {
    state
        .onera
        .remove_profile_member(parse_member(&member_id)?)
        .await?;
    Ok(())
}

/// Enable or disable a member in desired state only.
#[tauri::command]
pub async fn set_member_state(
    state: State<'_, AppState>,
    member_id: String,
    desired: String,
) -> CommandResult<serde_json::Value> {
    let desired = match desired.as_str() {
        "enabled" => onera_core::domain::profile::DesiredModState::Enabled,
        "disabled" => onera_core::domain::profile::DesiredModState::Disabled,
        other => {
            return Err(CommandError {
                code: "internal".into(),
                message: format!("{other:?} is not a desired mod state"),
            })
        }
    };
    let member = state
        .onera
        .set_member_state(parse_member(&member_id)?, desired)
        .await?;
    Ok(serde_json::to_value(&member).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
pub async fn set_member_pin(
    state: State<'_, AppState>,
    member_id: String,
    pinned: bool,
    reason: Option<String>,
) -> CommandResult<serde_json::Value> {
    let member = state
        .onera
        .set_member_pin(parse_member(&member_id)?, pinned, reason)
        .await?;
    Ok(serde_json::to_value(&member).unwrap_or(serde_json::Value::Null))
}

/// Move a member in the provider stack.
///
/// `priority` is a signed integer, not a list index: inserting between two
/// members does not renumber the profile.
#[tauri::command]
pub async fn reorder_profile_member(
    state: State<'_, AppState>,
    member_id: String,
    priority: i32,
) -> CommandResult<serde_json::Value> {
    let member = state
        .onera
        .reorder_profile_member(
            parse_member(&member_id)?,
            onera_core::domain::profile::MemberPriority(priority),
        )
        .await?;
    Ok(serde_json::to_value(&member).unwrap_or(serde_json::Value::Null))
}

/// Dependency health currently known for a profile.
///
/// Until Milestone 4 supplies provider ingestion and the solver, this returns
/// the conservative unsupported/unknown result produced by the application
/// layer rather than leaving the desktop command unresolved.
#[tauri::command]
pub async fn resolve_dependencies(
    state: State<'_, AppState>,
    profile_id: String,
    preview_members: Option<serde_json::Value>,
) -> CommandResult<serde_json::Value> {
    let profile = parse_profile(&profile_id)?;
    // Checking an edit that has not been saved needs an application method that
    // takes it. Answering with the *committed* profile's result instead would
    // silently show the user a check of something they did not ask about.
    if preview_members
        .as_ref()
        .and_then(serde_json::Value::as_array)
        .is_some_and(|edits| !edits.is_empty())
    {
        return Err(awaiting("resolve_profile_dependencies_with_preview"));
    }
    let resolution = state.onera.resolve_profile_dependencies(profile).await?;
    Ok(serde_json::to_value(&resolution).unwrap_or(serde_json::Value::Null))
}

/// The application method a command needs before it can answer.
///
/// Milestone 4's solver and its application wiring land separately from this
/// adapter. Until they do, these commands parse and validate their arguments
/// and then refuse, with the stable code `unimplemented`. The alternative —
/// returning an empty or optimistic payload — would be exactly the
/// "unknown rendered as nothing" failure the dependency contract exists to
/// prevent, and computing an answer here would put dependency logic in a
/// driver.
fn awaiting(method: &str) -> CommandError {
    CommandError {
        code: "unimplemented".into(),
        message: format!(
            "this command needs Onera::{method}, which Milestone 4 has not wired up yet. \
             No dependency answer is invented here."
        ),
    }
}

fn parse_group(id: &str) -> CommandResult<onera_core::ids::DependencyGroupId> {
    onera_core::ids::DependencyGroupId::from_str(id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid dependency group id".into(),
    })
}

/// The raw requirement list a provider declared for one file.
///
/// The snapshot's `raw` field is diagnostic, can reach megabytes, and is never
/// rendered, so it is stripped at this boundary rather than sent to the webview.
#[tauri::command]
pub async fn dependency_snapshot(
    _state: State<'_, AppState>,
    mod_id: String,
    provider_file_id: String,
) -> CommandResult<serde_json::Value> {
    onera_core::ids::ModId::from_str(&mod_id).map_err(|_| CommandError {
        code: "internal".into(),
        message: "that is not a valid mod id".into(),
    })?;
    if provider_file_id.is_empty() {
        return Err(CommandError {
            code: "internal".into(),
            message: "a provider file id is required".into(),
        });
    }
    Err(awaiting("dependency_snapshot"))
}

/// Accept a solved dependency plan as a desired-state edit.
///
/// Nothing on disk changes: the profile is edited, and the reconciliation that
/// deploys it is still previewed and applied separately. A plan whose
/// fingerprint no longer matches is refused with `conflict`.
#[tauri::command]
pub async fn apply_dependency_plan(
    _state: State<'_, AppState>,
    profile_id: String,
    expected_fingerprint: Option<String>,
) -> CommandResult<serde_json::Value> {
    parse_profile(&profile_id)?;
    let _ = expected_fingerprint;
    Err(awaiting("apply_dependency_plan"))
}

/// Record that the user accepted one named dependency risk.
///
/// `fingerprint` is the definition that was displayed, not a fresh one: an
/// override must stop applying when the provider changes the requirement.
/// `reason` is required, because ignoring a requirement is always an
/// attributable decision.
#[tauri::command]
pub async fn set_dependency_override(
    _state: State<'_, AppState>,
    member_id: String,
    group_id: String,
    fingerprint: String,
    reason: String,
) -> CommandResult<serde_json::Value> {
    parse_member(&member_id)?;
    parse_group(&group_id)?;
    if fingerprint.trim().is_empty() {
        return Err(CommandError {
            code: "decision_required".into(),
            message: "the fingerprint that was displayed is required".into(),
        });
    }
    if reason.trim().is_empty() {
        return Err(CommandError {
            code: "decision_required".into(),
            message: "a reason is required to ignore a requirement".into(),
        });
    }
    Err(awaiting("set_dependency_override"))
}

/// Withdraw a previously accepted dependency risk.
#[tauri::command]
pub async fn clear_dependency_override(
    _state: State<'_, AppState>,
    member_id: String,
    group_id: String,
    fingerprint: String,
) -> CommandResult<()> {
    parse_member(&member_id)?;
    parse_group(&group_id)?;
    let _ = fingerprint;
    Err(awaiting("clear_dependency_override"))
}

/// Solve the whole enabled profile for one compatible update set.
///
/// "Update all compatible" is one solve of the entire profile, not a newest
/// version chosen per mod, so it produces one preview and one journaled
/// operation.
#[tauri::command]
pub async fn plan_compatible_updates(
    _state: State<'_, AppState>,
    profile_id: String,
) -> CommandResult<serde_json::Value> {
    parse_profile(&profile_id)?;
    Err(awaiting("plan_compatible_updates"))
}

/// Apply the whole-profile compatible update set that was previewed.
#[tauri::command]
pub async fn apply_compatible_updates(
    _state: State<'_, AppState>,
    profile_id: String,
    expected_fingerprint: Option<String>,
) -> CommandResult<serde_json::Value> {
    parse_profile(&profile_id)?;
    let _ = expected_fingerprint;
    Err(awaiting("apply_compatible_updates"))
}

/// Preview a profile switch without touching the game directory.
///
/// `ready` is false whenever `blockers` is non-empty. Cross-mod conflicts are
/// resolved with `decide` and stay separate from dependency problems: accepting
/// a dependency risk never picks a winner for a path conflict.
#[tauri::command]
pub async fn plan_profile_activation(
    state: State<'_, AppState>,
    profile_id: String,
) -> CommandResult<serde_json::Value> {
    let preview = state
        .onera
        .plan_profile_activation(parse_profile(&profile_id)?)
        .await?;
    Ok(serde_json::to_value(&preview).unwrap_or(serde_json::Value::Null))
}

/// Apply a profile switch.
///
/// `expectedFingerprint` is the digest carried by the preview the user
/// approved; sending it back turns a desired state that moved in the meantime
/// into `conflict` instead of a silently different apply. The returned record
/// reports the target profile active only in `applied`, which is reached after
/// the written files have been re-hashed.
#[tauri::command]
pub async fn activate_profile(
    state: State<'_, AppState>,
    profile_id: String,
    expected_fingerprint: Option<String>,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let progress = state.progress();
    let activation = state
        .onera
        .activate_profile(
            parse_profile(&profile_id)?,
            expected_fingerprint.as_deref(),
            &progress,
            &cancel,
        )
        .await?;
    Ok(serde_json::to_value(&activation).unwrap_or(serde_json::Value::Null))
}

/// Recent activation attempts for a game, newest first.
#[tauri::command]
pub async fn profile_activation_history(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    let history = state
        .onera
        .profile_activation_history(parse_game(&game_id)?, 20)
        .await?;
    Ok(serde_json::to_value(&history).unwrap_or(serde_json::Value::Null))
}

// ---------------------------------------------------------------------------
// Baseline
// ---------------------------------------------------------------------------

fn parse_baseline_source(source: Option<String>) -> CommandResult<Option<BaselineSource>> {
    let Some(source) = source else {
        return Ok(None);
    };
    Ok(Some(match source.as_str() {
        "store_verified_capture" => BaselineSource::StoreVerifiedCapture,
        "local_snapshot" => BaselineSource::LocalSnapshot,
        "store_manifest" => BaselineSource::StoreManifest,
        other => {
            return Err(CommandError {
                code: "internal".into(),
                message: format!("{other:?} is not a baseline source"),
            })
        }
    }))
}

#[tauri::command]
pub async fn baseline_status(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    let report = state.onera.baseline_status(parse_game(&game_id)?).await?;
    Ok(serde_json::to_value(&report).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
pub async fn plan_baseline_capture(
    state: State<'_, AppState>,
    game_id: String,
    source: Option<String>,
) -> CommandResult<serde_json::Value> {
    let preview = state
        .onera
        .plan_baseline_capture(parse_game(&game_id)?, parse_baseline_source(source)?)
        .await?;
    Ok(serde_json::to_value(&preview).unwrap_or(serde_json::Value::Null))
}

/// Capture a baseline.
///
/// `storeVerificationConfirmed` is the user's explicit acknowledgement that they
/// ran the store's own file verification. Onera cannot observe that, so a
/// store-verified capture without it returns `decision_required` rather than
/// silently recording a weaker claim as a stronger one.
#[tauri::command]
pub async fn capture_baseline(
    state: State<'_, AppState>,
    game_id: String,
    source: Option<String>,
    store_verification_confirmed: bool,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let progress = state.progress();
    let baseline = state
        .onera
        .capture_baseline(
            parse_game(&game_id)?,
            parse_baseline_source(source)?,
            store_verification_confirmed,
            &progress,
            &cancel,
        )
        .await?;
    Ok(serde_json::to_value(&baseline).unwrap_or(serde_json::Value::Null))
}

/// Compare an installation with its baseline.
///
/// `quick` returns `evidence: "metadata_only"`, which must never be rendered as
/// clean: only a completed, content-hashed scan over the captured scope can be.
#[tauri::command]
pub async fn verify_baseline(
    state: State<'_, AppState>,
    game_id: String,
    quick: bool,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let progress = state.progress();
    let verification = state
        .onera
        .verify_baseline(parse_game(&game_id)?, quick, &progress, &cancel)
        .await?;
    Ok(serde_json::to_value(&verification).unwrap_or(serde_json::Value::Null))
}

/// Preview reconciling to an empty active mod set, with baseline context.
///
/// `needs_store_repair` is reported, never repaired, and `unknown_extras` are
/// never deleted by this flow — with or without confirmation.
#[tauri::command]
pub async fn plan_return_to_clean(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let progress = state.progress();
    let preview = state
        .onera
        .plan_return_to_clean(parse_game(&game_id)?, &progress, &cancel)
        .await?;
    Ok(serde_json::to_value(&preview).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
pub async fn apply_return_to_clean(
    state: State<'_, AppState>,
    game_id: String,
) -> CommandResult<serde_json::Value> {
    let cancel = onera_core::progress::CancelToken::new();
    let progress = state.progress();
    let report = state
        .onera
        .apply_return_to_clean(parse_game(&game_id)?, &progress, &cancel)
        .await?;
    Ok(serde_json::to_value(&report).unwrap_or(serde_json::Value::Null))
}

fn removal_view(report: &onera_install::RemovalReport) -> serde_json::Value {
    let render = |items: &[TargetLocation]| {
        items
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    };
    json!({
        "deleted": render(&report.deleted),
        "restored": render(&report.restored),
        "kept_shared": render(&report.kept_shared),
        "already_missing": render(&report.already_missing),
        "externally_modified": render(&report.externally_modified),
        "directories_removed": report.directories_removed,
    })
}

#[tauri::command]
pub async fn preview_removal(
    state: State<'_, AppState>,
    game_id: String,
    installation_id: String,
) -> CommandResult<serde_json::Value> {
    let report = state
        .onera
        .preview_removal(parse_game(&game_id)?, parse_installation(&installation_id)?)
        .await?;
    Ok(removal_view(&report))
}

#[tauri::command]
pub async fn remove_mod(
    state: State<'_, AppState>,
    game_id: String,
    installation_id: String,
    force: bool,
) -> CommandResult<serde_json::Value> {
    let progress = state.progress();
    let cancel = onera_core::progress::CancelToken::new();
    let report = state
        .onera
        .remove(
            parse_game(&game_id)?,
            parse_installation(&installation_id)?,
            if force {
                ModifiedFilePolicy::Force
            } else {
                ModifiedFilePolicy::Ask
            },
            &progress,
            &cancel,
        )
        .await?;
    Ok(removal_view(&report))
}

#[tauri::command]
pub async fn ownership(
    state: State<'_, AppState>,
    game_id: String,
    root_key: String,
    path: String,
) -> CommandResult<serde_json::Value> {
    let stack = state
        .onera
        .ownership(
            parse_game(&game_id)?,
            &TargetLocation {
                root_key,
                path: RelPath::normalize(&path)?,
            },
        )
        .await?;
    Ok(json!({
        "entries": stack.entries().iter().map(|e| json!({
            "kind": if e.provider.is_unmanaged() { "unmanaged_backup" } else { "installation" },
            "installation_id": e.provider.installation_id().map(|i| i.to_string()),
            "mod_name": serde_json::Value::Null,
            "hash": e.hash.hex,
            "size": e.size,
        })).collect::<Vec<_>>(),
    }))
}

#[tauri::command]
pub async fn interrupted_operations(
    state: State<'_, AppState>,
) -> CommandResult<serde_json::Value> {
    let items = state.onera.interrupted_operations().await?;
    Ok(json!(items
        .iter()
        .map(|i| json!({
            "operation_id": i.operation.id.to_string(),
            "kind": format!("{:?}", i.operation.kind).to_lowercase(),
            "state": i.operation.state.to_string(),
            "recovery": format!("{:?}", i.recovery),
            "committed_files": i.committed_files,
            "staged_files": i.staged_files,
            "created_at": i.operation.created_at.to_rfc3339(),
        }))
        .collect::<Vec<_>>()))
}

#[tauri::command]
pub async fn roll_back(state: State<'_, AppState>, operation_id: String) -> CommandResult<()> {
    let progress = state.progress();
    state
        .onera
        .roll_back(parse_operation(&operation_id)?, &progress)
        .await?;
    Ok(())
}

/// Paths and versions, for the diagnostics pane and for bug reports.
///
/// Deliberately contains no credential and no account identifier, so the pane
/// can be screenshotted safely.
#[tauri::command]
pub async fn diagnostics(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    let paths = &state.onera.paths;
    let _ = NullProgress;
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "database": paths.database().display().to_string(),
        "archives": paths.archives().display().to_string(),
        "backups": paths.backups().display().to_string(),
        "staging": paths.staging().display().to_string(),
        "logs": paths.logs().display().to_string(),
        "sevenzip": onera_archive::find_sevenz()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".to_owned()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use onera_core::CoreError;

    #[test]
    fn base64_matches_the_standard_encoding_including_padding() {
        // The three residues are the whole of the encoding's difficulty.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_covers_the_whole_byte_range() {
        // A PNG is not text: an encoder that only worked on ASCII would pass
        // every test above and still produce an unreadable image.
        let bytes: Vec<u8> = (0..=255_u8).collect();
        let encoded = base64(&bytes);
        assert_eq!(encoded.len(), 344);
        assert!(encoded.starts_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g"));
        assert!(encoded.ends_with("+/w=="));
    }

    #[test]
    fn identifier_arguments_are_parsed_not_trusted() {
        assert!(parse_profile(&onera_core::ids::ProfileId::new().to_string()).is_ok());
        let rejected = parse_profile("../../etc/passwd").unwrap_err();
        assert_eq!(rejected.code, "internal");
        assert!(parse_game("not-a-uuid").is_err());
    }

    #[test]
    fn the_activation_refusals_carry_the_codes_the_contract_names() {
        // A preview that is not ready.
        let blocked: CommandError = CoreError::DecisionRequired("2 blockers".into()).into();
        assert_eq!(blocked.code, "decision_required");
        // A preview whose desired state moved underneath it.
        let stale: CommandError = CoreError::Conflict("the preview is out of date".into()).into();
        assert_eq!(stale.code, "conflict");
        // An unknown profile.
        let missing: CommandError = CoreError::NotFound {
            kind: "profile",
            id: "x".into(),
        }
        .into();
        assert_eq!(missing.code, "not_found");
    }

    #[test]
    fn dependency_group_ids_are_parsed_not_trusted() {
        assert!(parse_group(&onera_core::ids::DependencyGroupId::new().to_string()).is_ok());
        assert_eq!(
            parse_group("../../etc/passwd").unwrap_err().code,
            "internal"
        );
    }

    #[test]
    fn an_unwired_dependency_command_refuses_instead_of_answering_emptily() {
        let refusal = awaiting("dependency_snapshot");
        assert_eq!(refusal.code, "unimplemented");
        assert!(refusal.message.contains("dependency_snapshot"));
        // The refusal must never read as "this mod requires nothing".
        assert!(!refusal.message.contains("no dependencies"));
    }
}
