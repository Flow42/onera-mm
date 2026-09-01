//! Tauri commands.
//!
//! Each function does three things and nothing else: parse its arguments,
//! call one application method, and shape the result for the frontend. Any
//! decision more interesting than that belongs in [`onera_app`] or deeper.

use crate::state::{AppState, CommandError, CommandResult};
use onera_core::ids::{InstallationId, LocalGameId, OperationId, ProviderFileId, ProviderModId};
use onera_core::plan::{ConflictChoice, Decision, DecisionScope, InstallPlan, TargetLocation};
use onera_core::progress::NullProgress;
use onera_core::redact::Secret;
use onera_core::RelPath;
use onera_install::remove::ModifiedFilePolicy;
use serde_json::json;
use std::str::FromStr;
use tauri::State;

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
    Ok(json!({
        "authenticated": state.onera.is_authenticated().await?,
        "recovery_required": !state.onera.interrupted_operations().await?.is_empty(),
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
    Ok(serde_json::to_value(
        found
            .iter()
            .map(|g| {
                json!({
                    "adapter_id": g.adapter_id,
                    "provider_slug": g.provider_slug,
                    "name": g.name,
                    "install_root": g.install_root,
                    "compat_prefix": g.compat_prefix,
                    "user_data_roots": g.user_data_roots,
                    "source": format!("{:?}", g.source).to_lowercase(),
                    "validation": {
                        "valid": g.validation.valid,
                        "reported_version": g.validation.reported_version,
                        "findings": g.validation.findings,
                    },
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or(serde_json::Value::Null))
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
    Ok(serde_json::to_value(
        state.onera.installed_mods(parse_game(&game_id)?).await?,
    )
    .unwrap_or(serde_json::Value::Null))
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

#[tauri::command]
pub async fn inbox_requests(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    Ok(serde_json::to_value(state.onera.inbox_requests().await?)
        .unwrap_or(serde_json::Value::Null))
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
    state
        .onera
        .dismiss_inbox_request(id)
        .await?;
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

#[tauri::command]
pub async fn downloads(state: State<'_, AppState>) -> CommandResult<serde_json::Value> {
    Ok(serde_json::to_value(state.onera.downloads().await?)
        .unwrap_or(serde_json::Value::Null))
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
