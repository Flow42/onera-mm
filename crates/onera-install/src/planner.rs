//! Dry-run installation planning.
//!
//! The planner reads the world and writes nothing. It takes the archive
//! manifest, the layout mapping the game adapter produced, and the recorded and
//! on-disk state of every target, and returns an [`InstallPlan`] whose entries
//! are already classified.
//!
//! Planning is deliberately re-runnable: the same inputs always produce the same
//! plan, so a preview the user is looking at can be regenerated and compared
//! before it is applied.

use onera_core::domain::archive::ArchiveManifest;
use onera_core::ids::{InstallationId, LocalGameId, ModId, OperationId};
use onera_core::plan::{
    classify, FileClassification, InstallPlan, PlannedFile, ScopedRule, TargetLocation, TargetState,
};
use onera_core::ports::{DeploymentStore, FileSystem, GameAdapter};
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::{CoreError, RelPath, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Absolute directories for each deployment-root key.
pub type RootMap = HashMap<String, PathBuf>;

/// Everything the planner needs to describe one install.
pub struct PlanRequest<'a> {
    /// Game installation being written to.
    pub local_game_id: LocalGameId,
    /// Mod lineage being installed.
    pub mod_id: ModId,
    /// Identity the new installation will have.
    pub installation_id: InstallationId,
    /// Manifest of the extracted archive.
    pub manifest: &'a ArchiveManifest,
    /// Source-to-target mapping from the game adapter.
    pub mappings: &'a [(RelPath, TargetLocation)],
    /// Absolute path of each deployment root.
    pub roots: &'a RootMap,
    /// The adapter, used to validate targets.
    pub adapter: &'a dyn GameAdapter,
    /// Remembered rules for this mod.
    pub rules: &'a [ScopedRule],
}

/// Build a plan without touching anything.
///
/// # Errors
/// Fails if a mapping names an unknown deployment root, if a source path is not
/// in the manifest, or if reading current state fails.
pub async fn plan_install(
    request: PlanRequest<'_>,
    fs: &dyn FileSystem,
    deployments: &dyn DeploymentStore,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<InstallPlan> {
    progress.emit(ProgressEvent::Started {
        operation: None,
        stage: Stage::Planning,
        total: Some(request.mappings.len() as u64),
    });

    // Installations of the same mod are how "a previous release of this mod" is
    // recognized. Nothing provider-specific is involved.
    let same_mod: HashSet<InstallationId> = deployments
        .installations_of_mod(request.local_game_id, request.mod_id)
        .await?
        .into_iter()
        .collect();

    let mut files = Vec::with_capacity(request.mappings.len());
    let mut seen_case_folded: HashMap<String, RelPath> = HashMap::new();

    for (index, (source, target)) in request.mappings.iter().enumerate() {
        cancel.check()?;

        let Some(manifest_file) = request.manifest.file(source) else {
            return Err(CoreError::InvalidInput(format!(
                "layout maps {source}, which the archive manifest does not contain"
            )));
        };
        let mut notes = Vec::new();

        // Two archive entries that differ only in case would collide inside a
        // Proton prefix or on a case-insensitive mount.
        let fold_key = format!("{}:{}", target.root_key, target.path.case_fold_key());
        if let Some(previous) = seen_case_folded.insert(fold_key, target.path.clone()) {
            if previous != target.path {
                notes.push(format!(
                    "collides with {previous} on case-insensitive filesystems"
                ));
            }
        }

        let classification_override = if let Err(e) = request.adapter.validate_target(target) {
            notes.push(e.to_string());
            Some(FileClassification::InvalidTarget)
        } else if request.roots.get(&target.root_key).is_none() {
            notes.push(format!("no deployment root named {:?}", target.root_key));
            Some(FileClassification::InvalidTarget)
        } else {
            None
        };

        let (classification, existing_hash, mut state_notes) = match classification_override {
            Some(c) => (c, None, Vec::new()),
            None => {
                let root = &request.roots[&target.root_key];
                let absolute = target.path.resolve_under(root);
                let on_disk = fs.stat_hash(&absolute).await?;
                let stack = deployments.stack(request.local_game_id, target).await?;
                let state = TargetState {
                    on_disk: on_disk.clone(),
                    stack,
                };
                let (c, n) = classify(&manifest_file.hash, &state, &same_mod);
                (c, on_disk.map(|(h, _)| h), n)
            }
        };
        notes.append(&mut state_notes);

        // A remembered rule can pre-resolve a conflict, but it can never create
        // one and never applies to anything but the classes that ask.
        let decision = classification
            .needs_decision()
            .then(|| {
                request
                    .rules
                    .iter()
                    .find(|r| r.matches(request.mod_id, target))
                    .map(|r| r.choice)
            })
            .flatten();
        if decision.is_some() {
            notes.push("resolved by a remembered rule".to_owned());
        }

        files.push(PlannedFile {
            source: source.clone(),
            target: target.clone(),
            source_hash: manifest_file.hash.clone(),
            source_size: manifest_file.size,
            classification,
            existing_hash,
            decision,
            notes,
        });

        progress.emit(ProgressEvent::Advanced {
            stage: Stage::Planning,
            completed: index as u64 + 1,
            total: Some(request.mappings.len() as u64),
            detail: Some(target.to_string()),
        });
    }

    let mut plan = InstallPlan {
        operation_id: OperationId::new(),
        local_game_id: request.local_game_id,
        installation_id: request.installation_id,
        mod_id: request.mod_id,
        files,
    };
    plan.sort();
    progress.emit(ProgressEvent::Finished {
        stage: Stage::Planning,
        success: true,
    });
    Ok(plan)
}

/// Render a plan as a human-readable dry-run preview.
///
/// Used by `onera install --dry-run` and by the UI's preview pane, so both show
/// exactly the same thing.
#[must_use]
pub fn render_preview(plan: &InstallPlan) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} file(s), {} byte(s) to write",
        plan.files.len(),
        plan.bytes_to_write()
    );
    for (classification, count) in plan.summary() {
        let _ = writeln!(out, "  {classification:?}: {count}");
    }
    let _ = writeln!(out);
    for file in &plan.files {
        let _ = writeln!(
            out,
            "  {:?} {} <- {}{}",
            file.effective_action(),
            file.target,
            file.source,
            if file.notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", file.notes.join("; "))
            }
        );
    }
    if !plan.is_ready() {
        let _ = writeln!(
            out,
            "\n{} decision(s) required before this can be applied",
            plan.unresolved().count()
        );
    }
    out
}
