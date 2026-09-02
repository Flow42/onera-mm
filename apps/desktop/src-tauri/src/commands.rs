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
}
