//! The Onera command-line interface.
//!
//! A thin adapter over [`onera_app::Onera`]. Every subcommand is a direct
//! translation of one application method; nothing here decides what to install,
//! what conflicts mean, or what to write. That keeps the CLI and the desktop
//! application incapable of disagreeing.
//!
//! The CLI is also the headless entry point: CI, packaging smoke tests and the
//! manual smoke test in `docs/recovery.md` all drive it.

#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use onera_app::{InstallRequest, Onera, Paths};
use onera_core::domain::baseline::{BaselineFreshness, BaselineSource};
use onera_core::domain::profile::{DesiredModState, MemberPriority};
use onera_core::ids::{
    InstallationId, LocalGameId, ModId, ProfileId, ProfileMemberId, ProviderFileId, ProviderModId,
};
use onera_core::plan::{ConflictChoice, Decision, DecisionScope};
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::redact::Secret;
use onera_install::remove::ModifiedFilePolicy;
use std::io::IsTerminal as _;
use std::str::FromStr;

#[derive(Debug, Parser)]
#[command(
    name = "onera",
    about = "A Linux-first, game-agnostic mod manager",
    version
)]
struct Cli {
    /// Print structured JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,

    /// Enable debug logging.
    #[arg(long, short, global = true)]
    verbose: bool,

    /// Use a different data root. Mainly for testing and portable installs.
    #[arg(long, global = true, env = "ONERA_ROOT")]
    root: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Store, replace, inspect or delete the Nexus personal API key.
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Find installed games Onera can manage.
    Discover,
    /// List registered game installations.
    Games,
    /// Show a mod's metadata and files.
    Mod {
        /// Nexus game domain, e.g. `cyberpunk2077`.
        game: String,
        /// Mod id from the mod page URL.
        mod_id: String,
    },
    /// Preview or perform an installation.
    Install {
        /// Registered game installation id, from `onera games`.
        #[arg(long)]
        game: String,
        /// Nexus game domain.
        #[arg(long)]
        domain: String,
        /// Mod id from the mod page URL.
        #[arg(long)]
        mod_id: String,
        /// A specific file id. Defaults to the mod page's primary file.
        #[arg(long)]
        file_id: Option<String>,
        /// Show the plan and stop. This is the default.
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        /// Actually write the files.
        #[arg(long)]
        apply: bool,
        /// Resolve every conflict the same way, rather than stopping.
        #[arg(long, value_name = "CHOICE")]
        on_conflict: Option<ConflictArg>,
    },
    /// Re-read every file an installation claims.
    Verify {
        /// Game installation id.
        #[arg(long)]
        game: String,
        /// Installation id.
        #[arg(long)]
        installation: String,
    },
    /// Remove an installation and restore what it covered.
    Remove {
        /// Game installation id.
        #[arg(long)]
        game: String,
        /// Installation id.
        #[arg(long)]
        installation: String,
        /// Show what would happen and stop.
        #[arg(long)]
        dry_run: bool,
        /// Remove files even if they changed since installation.
        #[arg(long)]
        force_modified: bool,
    },
    /// Preview a desired active-mod state.
    PlanState {
        /// Registered game installation id.
        #[arg(long)]
        game: String,
        /// Retained installation to enable; may be repeated.
        #[arg(long)]
        enable: Vec<String>,
        /// Active installation to disable; may be repeated.
        #[arg(long)]
        disable: Vec<String>,
        /// Resolve a collision as ROOT:PATH=INSTALLATION_ID; may be repeated.
        #[arg(long, value_name = "ROOT:PATH=INSTALLATION_ID")]
        winner: Vec<String>,
    },
    /// Apply a desired active-mod state as one transaction.
    ApplyState {
        /// Registered game installation id.
        #[arg(long)]
        game: String,
        /// Retained installation to enable; may be repeated.
        #[arg(long)]
        enable: Vec<String>,
        /// Active installation to disable; may be repeated.
        #[arg(long)]
        disable: Vec<String>,
        /// Resolve a collision as ROOT:PATH=INSTALLATION_ID; may be repeated.
        #[arg(long, value_name = "ROOT:PATH=INSTALLATION_ID")]
        winner: Vec<String>,
    },
    /// Enable one retained installation without downloading it again.
    Enable {
        /// Registered game installation id.
        #[arg(long)]
        game: String,
        /// Retained installation id.
        #[arg(long)]
        installation: String,
    },
    /// Disable one installation while retaining its artifact.
    Disable {
        /// Registered game installation id.
        #[arg(long)]
        game: String,
        /// Active installation id.
        #[arg(long)]
        installation: String,
    },
    /// Show who provides a deployed path, oldest first.
    Ownership {
        /// Game installation id.
        #[arg(long)]
        game: String,
        /// Deployment root key, e.g. `game`.
        #[arg(long, default_value = "game")]
        root: String,
        /// Path relative to that root.
        path: String,
    },
    /// List and resolve operations that were interrupted.
    Recover {
        /// Roll back every interrupted operation.
        #[arg(long)]
        rollback: bool,
    },
    /// Inspect, capture and verify a game's clean-state baseline.
    Baseline {
        #[command(subcommand)]
        action: BaselineAction,
    },
    /// Configure browser Native Messaging for portable/AppImage installs.
    Browser {
        #[command(subcommand)]
        action: BrowserAction,
    },
    /// Manage reusable desired-state profiles without activating them.
    Profiles {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Inspect and decide on provider-declared dependencies.
    Deps {
        #[command(subcommand)]
        action: DepsAction,
    },
}

/// Dependency subcommands.
///
/// These print exactly the payloads `docs/frontend-contracts.md` documents, so
/// the contract stays honest without a compiled desktop binary. Every one of
/// them parses its input, calls a single application method and serialises the
/// result; none of them decides anything itself.
#[derive(Debug, Subcommand)]
enum DepsAction {
    /// Solve a profile's dependencies and report the result.
    ///
    /// The `--preview-*` options describe desired-state edits that have not
    /// been made, so a change can be checked before it is saved. Nothing they
    /// name is written.
    Check {
        /// Profile id.
        #[arg(long)]
        profile: String,
        /// Uncommitted addition, as `<mod-id>` or `<mod-id>:<provider-file-id>`.
        #[arg(long = "preview-add", value_name = "MOD[:FILE]")]
        preview_add: Vec<String>,
        /// Uncommitted enable of an existing member.
        #[arg(long = "preview-enable", value_name = "MEMBER")]
        preview_enable: Vec<String>,
        /// Uncommitted disable of an existing member.
        #[arg(long = "preview-disable", value_name = "MEMBER")]
        preview_disable: Vec<String>,
        /// Uncommitted pin of an existing member.
        #[arg(long = "preview-pin", value_name = "MEMBER")]
        preview_pin: Vec<String>,
        /// Uncommitted unpin of an existing member.
        #[arg(long = "preview-unpin", value_name = "MEMBER")]
        preview_unpin: Vec<String>,
    },
    /// Print the requirement list a provider declared for one file.
    Show {
        /// Onera mod-lineage id.
        #[arg(long = "mod")]
        mod_id: String,
        /// Opaque provider file id.
        #[arg(long = "file")]
        provider_file: String,
    },
    /// Accept, or withdraw, one named dependency risk.
    ///
    /// The fingerprint is the one that was displayed with the requirement. An
    /// override is scoped to that exact definition, so a provider that changes
    /// the requirement invalidates the decision instead of silently keeping it.
    Ignore {
        /// Profile member id.
        #[arg(long)]
        member: String,
        /// Dependency group id.
        #[arg(long)]
        group: String,
        /// Fingerprint of the definition that was displayed.
        #[arg(long)]
        fingerprint: String,
        /// Why the risk is acceptable. Required unless clearing.
        #[arg(long, required_unless_present = "clear")]
        reason: Option<String>,
        /// Withdraw a previously accepted risk instead of recording one.
        #[arg(long, conflicts_with = "reason")]
        clear: bool,
    },
    /// Accept a solved dependency plan as a desired-state edit.
    ApplyPlan {
        /// Profile id.
        #[arg(long)]
        profile: String,
        /// Fingerprint of the proposal you approved.
        #[arg(long)]
        expect: Option<String>,
    },
    /// Solve the whole enabled profile for one compatible update set.
    PlanUpdates {
        /// Profile id.
        #[arg(long)]
        profile: String,
    },
    /// Apply the whole-profile compatible update set.
    ApplyUpdates {
        /// Profile id.
        #[arg(long)]
        profile: String,
        /// Fingerprint of the preview you approved.
        #[arg(long)]
        expect: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum BaselineAction {
    /// Show the baseline, its freshness, and whether a capture can start.
    Status {
        /// Registered game installation id.
        #[arg(long)]
        game: String,
    },
    /// Hash the store-managed scope and record it as the current baseline.
    Capture {
        /// Registered game installation id.
        #[arg(long)]
        game: String,
        /// Confirm that the store's own file verification was run and finished.
        ///
        /// Onera cannot check this itself, so a store-verified capture refuses
        /// to start without it.
        #[arg(long)]
        verified: bool,
        /// Record a clearly labelled local snapshot instead.
        #[arg(long, conflicts_with = "verified")]
        local_snapshot: bool,
        /// Show what would be scanned and stop.
        #[arg(long)]
        dry_run: bool,
    },
    /// Compare the installation with its baseline.
    Verify {
        /// Registered game installation id.
        #[arg(long)]
        game: String,
        /// Compare sizes and modes only. Fast, and never reports clean.
        #[arg(long)]
        quick: bool,
    },
    /// Remove every active Onera mod and restore what Onera backed up.
    ///
    /// Never deletes a file Onera did not deploy, and never invents content for
    /// a damaged game file: both are reported for you or the store to resolve.
    Clean {
        /// Registered game installation id.
        #[arg(long)]
        game: String,
        /// Actually apply the restore. Without it, only the preview is printed.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
enum AuthAction {
    /// Store an API key, reading it from stdin so it never lands in shell history.
    Login,
    /// Show which account the stored key belongs to.
    Status,
    /// Delete the stored key.
    Logout,
}

#[derive(Debug, Subcommand)]
enum BrowserAction {
    /// Install a per-user Native Messaging host manifest.
    Setup {
        /// Browser whose per-user configuration should be updated.
        #[arg(long, value_enum, default_value_t = Browser::Chromium)]
        browser: Browser,
        /// Absolute path to the onera-nmhost executable.
        #[arg(long, default_value = "/usr/lib/onera/onera-nmhost")]
        host_path: std::path::PathBuf,
    },
    /// Print a Native Messaging manifest without writing it.
    Manifest {
        /// Absolute path to the onera-nmhost executable.
        #[arg(long, default_value = "/usr/lib/onera/onera-nmhost")]
        host_path: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileAction {
    /// List every profile for a registered local game.
    List {
        /// Registered local game installation id.
        #[arg(long)]
        game: String,
    },
    /// Create an empty profile or duplicate an existing profile.
    Create {
        /// Registered local game installation id.
        #[arg(long)]
        game: String,
        /// Per-game unique display name.
        #[arg(long)]
        name: String,
        /// Optional profile note.
        #[arg(long)]
        description: Option<String>,
        /// Existing profile in the same game to duplicate.
        #[arg(long = "from-profile")]
        from_profile: Option<String>,
    },
    /// Rename a profile.
    Rename {
        /// Profile id.
        profile: String,
        /// New per-game unique display name.
        #[arg(long)]
        name: String,
    },
    /// Delete an inactive profile.
    Delete {
        /// Profile id.
        profile: String,
    },
    /// Show one profile and its priority-ordered members.
    Show {
        /// Profile id.
        profile: String,
    },
    /// Add a mod lineage to a profile's desired state.
    Add {
        /// Profile id.
        profile: String,
        /// Onera mod-lineage id.
        #[arg(long = "mod")]
        mod_id: String,
        /// Optional opaque provider file id.
        #[arg(long = "file")]
        provider_file: Option<String>,
    },
    /// Remove a member from its profile.
    Remove {
        /// Profile member id.
        member: String,
    },
    /// Mark a member enabled in desired state.
    Enable {
        /// Profile member id.
        member: String,
    },
    /// Mark a member disabled in desired state.
    Disable {
        /// Profile member id.
        member: String,
    },
    /// Pin a member's selected provider version, or unpin it.
    Pin {
        /// Profile member id.
        member: String,
        /// Optional explanation for the pin.
        #[arg(long)]
        reason: Option<String>,
        /// Remove the existing pin.
        #[arg(long, conflicts_with = "reason")]
        unpin: bool,
    },
    /// Assign a signed provider-stack priority to a member.
    Reorder {
        /// Profile member id.
        member: String,
        /// Lower values deploy first.
        #[arg(long, allow_hyphen_values = true)]
        priority: i32,
    },
    /// Preview everything switching to this profile entails, changing nothing.
    PlanActivate {
        /// Profile id.
        #[arg(long)]
        profile: String,
    },
    /// Switch the game to this profile.
    ///
    /// Missing artifacts are downloaded first; the game directory only changes
    /// in the journaled reconciliation that follows, and the profile is
    /// reported active only once the files on disk match it.
    Activate {
        /// Profile id.
        #[arg(long)]
        profile: String,
        /// Fingerprint from the preview you approved.
        ///
        /// Refuses to apply if the desired state or the deployment moved since,
        /// instead of quietly applying a different plan.
        #[arg(long)]
        expect: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Browser {
    Chromium,
    Chrome,
    Brave,
}

const EXTENSION_ID: &str = "pohiidkpoflhifciokepgpaandghjgmj";

#[derive(Debug, Clone, Copy)]
enum ConflictArg {
    Keep,
    Replace,
    Adopt,
}

impl FromStr for ConflictArg {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "keep" => Ok(Self::Keep),
            "replace" => Ok(Self::Replace),
            "adopt" => Ok(Self::Adopt),
            other => Err(format!("expected keep, replace or adopt; got {other:?}")),
        }
    }
}

impl From<ConflictArg> for ConflictChoice {
    fn from(value: ConflictArg) -> Self {
        match value {
            ConflictArg::Keep => Self::KeepExisting,
            ConflictArg::Replace => Self::ReplaceAfterBackup,
            ConflictArg::Adopt => Self::AdoptExisting,
        }
    }
}

/// Renders progress as one line per stage change, or as JSON events.
struct CliProgress {
    json: bool,
    quiet: bool,
}

impl ProgressSink for CliProgress {
    fn emit(&self, event: ProgressEvent) {
        if self.quiet {
            return;
        }
        if self.json {
            if let Ok(line) = serde_json::to_string(&event) {
                println!("{line}");
            }
            return;
        }
        match event {
            ProgressEvent::Started { stage, total, .. } => {
                eprintln!(
                    "{}{}",
                    stage_label(stage),
                    total.map_or(String::new(), |t| format!(" ({t})"))
                );
            }
            ProgressEvent::Warning { message } => eprintln!("  warning: {message}"),
            // Per-item advances are far too noisy for a terminal; the stage
            // transitions are what a user actually wants to see.
            ProgressEvent::Advanced { .. } | ProgressEvent::Finished { .. } => {}
        }
    }
}

fn stage_label(stage: Stage) -> &'static str {
    match stage {
        Stage::Inspecting => "inspecting archive",
        Stage::Downloading => "downloading",
        Stage::Extracting => "extracting",
        Stage::Hashing => "hashing",
        Stage::Planning => "planning",
        Stage::BackingUp => "preparing",
        Stage::Deploying => "deploying",
        Stage::Verifying => "verifying",
        Stage::Removing => "removing",
        Stage::RollingBack => "rolling back",
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = match &cli.root {
        Some(root) => Paths::rooted_at(root.clone()),
        None => Paths::discover().context("cannot resolve XDG directories")?,
    };
    onera_app::logging::init(
        Some(&paths.logs()),
        if cli.json {
            onera_app::logging::LogFormat::Json
        } else {
            onera_app::logging::LogFormat::Text
        },
        cli.verbose,
    )
    .context("cannot initialize logging")?;

    let onera = Onera::new(paths).await.context("cannot start Onera")?;
    let cancel = CancelToken::new();
    let progress = CliProgress {
        json: cli.json,
        quiet: false,
    };

    match cli.command {
        Commands::Auth { action } => auth(&onera, action, cli.json).await,
        Commands::Discover => discover(&onera, &cancel, cli.json).await,
        Commands::Games => games(&onera, cli.json).await,
        Commands::Mod { game, mod_id } => show_mod(&onera, &game, &mod_id, &cancel, cli.json).await,
        Commands::Install {
            game,
            domain,
            mod_id,
            file_id,
            dry_run,
            apply,
            on_conflict,
        } => {
            install(
                &onera,
                InstallArgs {
                    game,
                    domain,
                    mod_id,
                    file_id,
                    apply: apply && !dry_run,
                    on_conflict,
                },
                &progress,
                &cancel,
                cli.json,
            )
            .await
        }
        Commands::Verify { game, installation } => {
            verify(&onera, &game, &installation, &progress, &cancel, cli.json).await
        }
        Commands::Remove {
            game,
            installation,
            dry_run,
            force_modified,
        } => {
            remove(
                &onera,
                &game,
                &installation,
                dry_run,
                force_modified,
                &progress,
                &cancel,
                cli.json,
            )
            .await
        }
        Commands::PlanState {
            game,
            enable,
            disable,
            winner,
        } => {
            state_change(
                &onera, &game, &enable, &disable, &winner, false, &progress, &cancel, cli.json,
            )
            .await
        }
        Commands::ApplyState {
            game,
            enable,
            disable,
            winner,
        } => {
            state_change(
                &onera, &game, &enable, &disable, &winner, true, &progress, &cancel, cli.json,
            )
            .await
        }
        Commands::Enable { game, installation } => {
            let game = LocalGameId::from_str(&game).context("invalid game id")?;
            let installation =
                InstallationId::from_str(&installation).context("invalid installation id")?;
            onera.enable(game, installation, &progress, &cancel).await?;
            emit(
                cli.json,
                &serde_json::json!({ "installation": installation, "active": true }),
                || format!("enabled {installation}"),
            );
            Ok(())
        }
        Commands::Disable { game, installation } => {
            let game = LocalGameId::from_str(&game).context("invalid game id")?;
            let installation =
                InstallationId::from_str(&installation).context("invalid installation id")?;
            onera
                .disable(game, installation, &progress, &cancel)
                .await?;
            emit(
                cli.json,
                &serde_json::json!({ "installation": installation, "active": false }),
                || format!("disabled {installation}"),
            );
            Ok(())
        }
        Commands::Ownership { game, root, path } => {
            ownership(&onera, &game, &root, &path, cli.json).await
        }
        Commands::Baseline { action } => {
            baseline(&onera, action, &progress, &cancel, cli.json).await
        }
        Commands::Recover { rollback } => recover(&onera, rollback, &progress, cli.json).await,
        Commands::Browser { action } => browser(action, cli.json).await,
        Commands::Profiles { action } => {
            profiles(&onera, action, &progress, &cancel, cli.json).await
        }
        Commands::Deps { action } => deps(&onera, action, cli.json).await,
    }
}

/// The application method a command needs before it can do anything.
///
/// Milestone 4's solver and its application wiring land separately from this
/// adapter. Until they do, a command that would have to invent an answer says
/// so and exits non-zero: an empty or optimistic result would be a lie, and the
/// driver is not allowed to compute one itself.
fn awaiting(method: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "this command needs `Onera::{method}`, which Milestone 4 has not wired up yet. \
         No dependency answer is invented here, so nothing is printed."
    )
}

async fn deps(onera: &Onera, action: DepsAction, json: bool) -> Result<()> {
    match action {
        DepsAction::Check {
            profile,
            preview_add,
            preview_enable,
            preview_disable,
            preview_pin,
            preview_unpin,
        } => {
            let profile = ProfileId::from_str(&profile).context("invalid profile id")?;
            let previews = preview_add.len()
                + preview_enable.len()
                + preview_disable.len()
                + preview_pin.len()
                + preview_unpin.len();
            if previews > 0 {
                return Err(awaiting("resolve_profile_dependencies_with_preview"));
            }
            let result = onera.resolve_profile_dependencies(profile).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_resolution(&result);
            }
            Ok(())
        }
        DepsAction::Show { mod_id, .. } => {
            // Parsed rather than passed through: an id that is not one is
            // refused here, not somewhere that would treat it as a path.
            ModId::from_str(&mod_id).context("invalid mod id")?;
            Err(awaiting("dependency_snapshot"))
        }
        DepsAction::Ignore { member, .. } => {
            ProfileMemberId::from_str(&member).context("invalid member id")?;
            Err(awaiting("set_dependency_override"))
        }
        DepsAction::ApplyPlan { profile, .. } => {
            ProfileId::from_str(&profile).context("invalid profile id")?;
            Err(awaiting("apply_dependency_plan"))
        }
        DepsAction::PlanUpdates { profile } => {
            ProfileId::from_str(&profile).context("invalid profile id")?;
            Err(awaiting("plan_compatible_updates"))
        }
        DepsAction::ApplyUpdates { profile, .. } => {
            ProfileId::from_str(&profile).context("invalid profile id")?;
            Err(awaiting("apply_compatible_updates"))
        }
    }
}

/// One line naming what the solver concluded.
///
/// Split out from the printer so the rule it exists for can be tested: every
/// outcome names itself, and nothing that is not `Compatible` reads as if it
/// were.
fn resolution_headline(outcome: &onera_core::domain::dependency::ResolutionOutcome) -> String {
    use onera_core::domain::dependency::ResolutionOutcome as O;
    match outcome {
        O::Compatible => "compatible: no dependency action is required".to_owned(),
        O::InstallMissing { install } => {
            format!("install-missing: {} candidate(s) to add", install.len())
        }
        O::UpdateSet { select, install } => format!(
            "update-set: {} version change(s), {} addition(s)",
            select.len(),
            install.len()
        ),
        O::DisableSet { disable } => {
            format!("disable-set: {} member(s) would be disabled", disable.len())
        }
        O::Unsatisfied { requirements } => format!(
            "unsatisfied: {} requirement(s) cannot be met",
            requirements.len()
        ),
        O::Unknown { reason } => format!("unknown: {reason}"),
    }
}

/// Render a resolution the way the JSON payload reads.
///
/// `unknown` and `unsatisfied` are printed as themselves. Neither is collapsed
/// into "compatible", and an outcome this build does not recognise is printed
/// with its own tag rather than guessed at.
fn print_resolution(result: &onera_core::domain::dependency::ResolutionResult) {
    println!("{}", resolution_headline(&result.outcome));
    for member in &result.health {
        println!("  {}  {:?}", member.profile_member_id, member.health);
        for requirement in &member.unsatisfied {
            println!(
                "    requires {} ({}): {}",
                requirement
                    .label
                    .as_deref()
                    .unwrap_or("unnamed requirement"),
                requirement.group_id,
                requirement.explanation
            );
        }
    }
    let evidence = &result.evidence;
    println!(
        "evidence: {} fresh, {} cached, {} stale, {} unavailable, {} unsupported, {} unknown DLC",
        evidence.fresh,
        evidence.cached,
        evidence.stale,
        evidence.unavailable,
        evidence.unsupported,
        evidence.unknown_dlc
    );
    if !evidence.is_complete_and_current() {
        println!("this answer rests on incomplete data and is not current");
    }
}

async fn profiles(
    onera: &Onera,
    action: ProfileAction,
    progress: &CliProgress,
    cancel: &CancelToken,
    json: bool,
) -> Result<()> {
    match action {
        ProfileAction::List { game } => {
            let profiles = onera
                .profiles(LocalGameId::from_str(&game).context("invalid game id")?)
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&profiles)?);
            } else if profiles.is_empty() {
                println!("no profiles for this game");
            } else {
                for profile in profiles {
                    println!(
                        "{}  {}{}",
                        profile.id,
                        profile.name,
                        if profile.is_active { "  (active)" } else { "" }
                    );
                }
            }
        }
        ProfileAction::Create {
            game,
            name,
            description,
            from_profile,
        } => {
            let profile = onera
                .create_profile(
                    LocalGameId::from_str(&game).context("invalid game id")?,
                    name,
                    description,
                    from_profile
                        .as_deref()
                        .map(ProfileId::from_str)
                        .transpose()
                        .context("invalid source profile id")?,
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&profile)?);
            } else {
                println!("created {} ({})", profile.name, profile.id);
            }
        }
        ProfileAction::Rename { profile, name } => {
            let profile = onera
                .rename_profile(
                    ProfileId::from_str(&profile).context("invalid profile id")?,
                    name,
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&profile)?);
            } else {
                println!("renamed profile {} to {}", profile.id, profile.name);
            }
        }
        ProfileAction::Delete { profile } => {
            let profile = ProfileId::from_str(&profile).context("invalid profile id")?;
            onera.delete_profile(profile).await?;
            emit(
                json,
                &serde_json::json!({ "deleted_profile_id": profile }),
                || format!("deleted profile {profile}"),
            );
        }
        ProfileAction::Show { profile } => {
            let details = onera
                .profile_details(ProfileId::from_str(&profile).context("invalid profile id")?)
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&details)?);
            } else {
                println!(
                    "{} ({}){}",
                    details.profile.name,
                    details.profile.id,
                    if details.profile.is_active {
                        "  active"
                    } else {
                        ""
                    }
                );
                for member in details.members {
                    println!(
                        "  {}  priority={}  {:?}  mod={}",
                        member.id, member.priority.0, member.desired, member.mod_id
                    );
                }
            }
        }
        ProfileAction::Add {
            profile,
            mod_id,
            provider_file,
        } => {
            let member = onera
                .add_profile_member(
                    ProfileId::from_str(&profile).context("invalid profile id")?,
                    ModId::from_str(&mod_id).context("invalid mod id")?,
                    provider_file.map(ProviderFileId::new),
                )
                .await?;
            print_profile_member(json, &member, "added");
        }
        ProfileAction::Remove { member } => {
            let member = ProfileMemberId::from_str(&member).context("invalid member id")?;
            onera.remove_profile_member(member).await?;
            emit(
                json,
                &serde_json::json!({ "removed_member_id": member }),
                || format!("removed profile member {member}"),
            );
        }
        ProfileAction::Enable { member } => {
            let member = onera
                .set_member_state(
                    ProfileMemberId::from_str(&member).context("invalid member id")?,
                    DesiredModState::Enabled,
                )
                .await?;
            print_profile_member(json, &member, "enabled");
        }
        ProfileAction::Disable { member } => {
            let member = onera
                .set_member_state(
                    ProfileMemberId::from_str(&member).context("invalid member id")?,
                    DesiredModState::Disabled,
                )
                .await?;
            print_profile_member(json, &member, "disabled");
        }
        ProfileAction::Pin {
            member,
            reason,
            unpin,
        } => {
            let member = onera
                .set_member_pin(
                    ProfileMemberId::from_str(&member).context("invalid member id")?,
                    !unpin,
                    reason,
                )
                .await?;
            print_profile_member(json, &member, if unpin { "unpinned" } else { "pinned" });
        }
        ProfileAction::Reorder { member, priority } => {
            let member = onera
                .reorder_profile_member(
                    ProfileMemberId::from_str(&member).context("invalid member id")?,
                    MemberPriority(priority),
                )
                .await?;
            print_profile_member(json, &member, "reordered");
        }
        ProfileAction::PlanActivate { profile } => {
            let preview = onera
                .plan_profile_activation(
                    ProfileId::from_str(&profile).context("invalid profile id")?,
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&preview)?);
            } else {
                print_activation_preview(&preview);
            }
        }
        ProfileAction::Activate { profile, expect } => {
            let activation = onera
                .activate_profile(
                    ProfileId::from_str(&profile).context("invalid profile id")?,
                    expect.as_deref(),
                    progress,
                    cancel,
                )
                .await?;
            emit(json, &serde_json::to_value(&activation)?, || {
                format!(
                    "activation {:?}: {} is now active",
                    activation.state, activation.to_profile_id
                )
            });
        }
    }
    Ok(())
}

/// Print the activation preview the way the JSON payload reads.
///
/// Downloads are listed separately from blockers on purpose: a member whose
/// artifact is missing is work to do, not a reason the switch cannot happen.
fn print_activation_preview(preview: &onera_core::domain::profile::ProfileActivationPreview) {
    println!(
        "from: {}",
        preview
            .from_profile_id
            .map_or_else(|| "(none)".to_owned(), |id| id.to_string())
    );
    println!("to:   {}", preview.to_profile_id);
    println!(
        "{} file change(s), {} byte(s) to write",
        preview.plan.steps.len(),
        preview.bytes_to_write
    );
    for download in &preview.downloads {
        println!(
            "  download  {}  {}",
            download.name,
            download.bytes.map_or_else(
                || "size unknown".to_owned(),
                |bytes| format!("{bytes} bytes")
            )
        );
    }
    println!("baseline: {}", freshness_label(&preview.baseline_freshness));
    if preview.ready {
        println!("ready to apply");
    } else {
        println!("not ready:");
        for blocker in &preview.blockers {
            println!("  {}", serde_json::to_string(blocker).unwrap_or_default());
        }
    }
    println!("fingerprint: {}", preview.fingerprint);
}

fn print_profile_member(
    json: bool,
    member: &onera_core::domain::profile::ProfileMember,
    action: &str,
) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(member).unwrap_or_default()
        );
    } else {
        println!("{action} profile member {}", member.id);
    }
}

async fn browser(action: BrowserAction, json: bool) -> Result<()> {
    match action {
        BrowserAction::Setup { browser, host_path } => {
            let config =
                dirs::config_dir().context("cannot resolve the user configuration directory")?;
            let directory = match browser {
                Browser::Chromium => config.join("chromium/NativeMessagingHosts"),
                Browser::Chrome => config.join("google-chrome/NativeMessagingHosts"),
                Browser::Brave => config.join("BraveSoftware/Brave-Browser/NativeMessagingHosts"),
            };
            tokio::fs::create_dir_all(&directory).await?;
            let destination = directory.join("com.onera.host.json");
            let manifest = native_messaging_manifest(&absolute_path(host_path)?);
            tokio::fs::write(&destination, serde_json::to_vec_pretty(&manifest)?).await?;
            emit(
                json,
                &serde_json::json!({ "manifest": destination }),
                || format!("installed {}", destination.display()),
            );
        }
        BrowserAction::Manifest { host_path } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&native_messaging_manifest(&absolute_path(
                    host_path
                )?))?
            );
        }
    }
    Ok(())
}

fn absolute_path(path: std::path::PathBuf) -> Result<std::path::PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn native_messaging_manifest(host_path: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "name": "com.onera.host",
        "description": "Onera native messaging host",
        "path": host_path,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    })
}

async fn auth(onera: &Onera, action: AuthAction, json: bool) -> Result<()> {
    match action {
        AuthAction::Login => {
            // Reading from stdin keeps the key out of the process table and out
            // of shell history, which an `--api-key` flag could not.
            let key = if std::io::stdin().is_terminal() {
                rpassword_prompt()?
            } else {
                let mut buffer = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)?;
                buffer
            };
            let account = onera
                .set_api_key(Secret::new(key.trim()))
                .await
                .context("could not store the API key")?;
            emit(
                json,
                &serde_json::json!({
                    "username": account.username,
                    "premium": account.premium,
                }),
                || format!("signed in as {}", account.username),
            );
        }
        AuthAction::Status => {
            if !onera.is_authenticated().await? {
                emit(json, &serde_json::json!({ "authenticated": false }), || {
                    "not signed in; run `onera auth login`".to_owned()
                });
                return Ok(());
            }
            let account = onera.account().await?;
            emit(
                json,
                &serde_json::json!({ "authenticated": true, "username": account.username }),
                || format!("signed in as {}", account.username),
            );
        }
        AuthAction::Logout => {
            onera.forget_api_key().await?;
            emit(json, &serde_json::json!({ "authenticated": false }), || {
                "the stored API key has been deleted".to_owned()
            });
        }
    }
    Ok(())
}

/// Prompt for a key without echoing it.
///
/// A dependency-free implementation: the terminal is put into no-echo mode via
/// `stty`, which is available everywhere Onera runs.
fn rpassword_prompt() -> Result<String> {
    eprint!("Nexus API key (input hidden): ");
    let echo_off = std::process::Command::new("stty").arg("-echo").status();
    let mut key = String::new();
    let read = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut key);
    if echo_off.map(|s| s.success()).unwrap_or(false) {
        let _ = std::process::Command::new("stty").arg("echo").status();
    }
    eprintln!();
    read?;
    Ok(key)
}

async fn discover(onera: &Onera, cancel: &CancelToken, json: bool) -> Result<()> {
    let found = onera.discover_games(cancel).await?;
    if json {
        let rendered: Vec<_> = found
            .iter()
            .map(|g| {
                serde_json::json!({
                    "adapter": g.adapter_id,
                    "name": g.name,
                    "path": g.install_root,
                    "usable": g.is_usable(),
                    "source": format!("{:?}", g.source),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rendered)?);
        return Ok(());
    }

    if found.is_empty() {
        println!("no supported games found; add one manually with a path");
        return Ok(());
    }
    for game in &found {
        println!(
            "{} {}\n  {}\n  {}",
            if game.is_usable() { "[ok]  " } else { "[warn]" },
            game.name,
            game.install_root.display(),
            game.validation.findings.join("; ")
        );
    }
    println!("\nconfirm a game in the desktop application before installing into it");
    Ok(())
}

async fn games(onera: &Onera, json: bool) -> Result<()> {
    let installs = onera.local_games().await?;
    if json {
        let rendered: Vec<_> = installs
            .iter()
            .map(|g| {
                serde_json::json!({
                    "id": g.id.to_string(),
                    "adapter": g.adapter_id,
                    "path": g.install_root,
                    "confirmed": g.confirmed,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rendered)?);
        return Ok(());
    }
    if installs.is_empty() {
        println!("no games registered yet; run `onera discover`");
    }
    for game in installs {
        println!(
            "{}  {}  {}",
            game.id,
            game.adapter_id,
            game.install_root.display()
        );
    }
    Ok(())
}

async fn show_mod(
    onera: &Onera,
    game: &str,
    mod_id: &str,
    cancel: &CancelToken,
    json: bool,
) -> Result<()> {
    let details = onera
        .fetch_mod(game, &ProviderModId::new(mod_id), cancel)
        .await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": details.name,
                "author": details.author,
                "needs_selection": details.needs_file_selection(),
                "files": details.files.iter().map(|f| serde_json::json!({
                    "id": f.provider_file_id.as_str(),
                    "name": f.name,
                    "category": format!("{:?}", f.category),
                    "primary": f.is_primary,
                    "size": f.size_bytes,
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }

    println!(
        "{} by {}",
        details.name,
        details.author.as_deref().unwrap_or("unknown")
    );
    for file in &details.files {
        println!(
            "  {:<12} {:<10} {}{}",
            file.provider_file_id.as_str(),
            format!("{:?}", file.category),
            file.name,
            if file.is_primary { "  (primary)" } else { "" }
        );
    }
    if details.needs_file_selection() {
        println!("\nseveral files are plausible; pass --file-id to choose one");
    }
    Ok(())
}

struct InstallArgs {
    game: String,
    domain: String,
    mod_id: String,
    file_id: Option<String>,
    apply: bool,
    on_conflict: Option<ConflictArg>,
}

async fn install(
    onera: &Onera,
    args: InstallArgs,
    progress: &CliProgress,
    cancel: &CancelToken,
    json: bool,
) -> Result<()> {
    let game = LocalGameId::from_str(&args.game).context("invalid game id")?;
    let details = onera
        .fetch_mod(&args.domain, &ProviderModId::new(&args.mod_id), cancel)
        .await?;

    let file = match &args.file_id {
        Some(id) => details
            .files
            .iter()
            .find(|f| f.provider_file_id.as_str() == id)
            .context("no file with that id")?,
        None => details
            .primary_file()
            .or_else(|| details.selectable_files().next())
            .context("this mod offers no downloadable file; pass --file-id")?,
    };
    if args.file_id.is_none() && details.needs_file_selection() {
        anyhow::bail!(
            "{} offers several plausible files; pass --file-id (see `onera mod`)",
            details.name
        );
    }

    let release_id = details
        .releases
        .iter()
        .find(|r| r.id == file.release_id)
        .or_else(|| details.releases.first())
        .context("the mod has no releases")?
        .id;

    let mut prepared = onera
        .prepare_install(
            &InstallRequest {
                local_game_id: game,
                game_slug: args.domain.clone(),
                mod_id: details.mod_id,
                release_id,
                provider_mod_id: ProviderModId::new(&args.mod_id),
                provider_file_id: ProviderFileId::new(file.provider_file_id.as_str()),
                filename: file.name.clone(),
                expected_size: file.size_bytes,
                expected_hash: file.published_hash.clone(),
            },
            progress,
            cancel,
        )
        .await?;

    if let Some(choice) = args.on_conflict {
        // Applies to every class that asks, in this operation only. There is no
        // flag for a persistent global rule; those are deliberately narrow and
        // are created in the UI.
        for classification in [
            onera_core::plan::FileClassification::ConflictWithOtherMod,
            onera_core::plan::FileClassification::UnmanagedExisting,
            onera_core::plan::FileClassification::ExternallyModified,
        ] {
            let target = prepared.plan.files.first().map(|f| f.target.clone());
            if let Some(target) = target {
                prepared.plan.apply_decision(
                    &target,
                    &Decision {
                        choice: choice.into(),
                        scope: DecisionScope::EquivalentInThisOperation { classification },
                    },
                );
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "operation": prepared.plan.operation_id.to_string(),
                "installation": prepared.plan.installation_id.to_string(),
                "layout": prepared.layout_rationale,
                "ignored": prepared.ignored,
                "rejected": prepared.rejected_entries.len(),
                "ready": prepared.plan.is_ready(),
                "files": prepared.plan.files.iter().map(|f| serde_json::json!({
                    "target": f.target.to_string(),
                    "classification": f.classification,
                    "action": f.effective_action(),
                    "notes": f.notes,
                })).collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!("layout: {}", prepared.layout_rationale);
        if prepared.ignored > 0 {
            println!("ignored {} non-content file(s)", prepared.ignored);
        }
        for rejected in &prepared.rejected_entries {
            println!("rejected {}: {}", rejected.raw_path, rejected.reason);
        }
        print!("{}", onera_install::render_preview(&prepared.plan));
    }

    if !args.apply {
        if !json {
            println!("\nthis was a dry run; pass --apply to write these files");
        }
        return Ok(());
    }
    if !prepared.plan.is_ready() {
        anyhow::bail!(
            "{} file(s) need a decision; resolve them in the desktop application or pass --on-conflict",
            prepared.plan.unresolved().count()
        );
    }

    let report = onera.apply(&prepared, progress, cancel).await?;
    emit(
        json,
        &serde_json::json!({
            "installation": prepared.plan.installation_id.to_string(),
            "written": report.written,
            "shared": report.shared,
            "skipped": report.skipped,
            "backed_up": report.backed_up,
        }),
        || {
            format!(
                "installed {}: {} written, {} shared, {} skipped, {} backed up\ninstallation {}",
                details.name,
                report.written,
                report.shared,
                report.skipped,
                report.backed_up,
                prepared.plan.installation_id
            )
        },
    );
    Ok(())
}

async fn verify(
    onera: &Onera,
    game: &str,
    installation: &str,
    progress: &CliProgress,
    cancel: &CancelToken,
    json: bool,
) -> Result<()> {
    let report = onera
        .verify(
            LocalGameId::from_str(game).context("invalid game id")?,
            InstallationId::from_str(installation).context("invalid installation id")?,
            progress,
            cancel,
        )
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for (status, count) in report.counts() {
            println!("{status}: {count}");
        }
        for problem in report.problems() {
            println!("  {:?} {}", problem.status, problem.target);
        }
    }
    if !report.is_clean() {
        // A non-zero exit lets a script notice without parsing output.
        std::process::exit(2);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn state_change(
    onera: &Onera,
    game: &str,
    enable: &[String],
    disable: &[String],
    winners: &[String],
    apply: bool,
    progress: &CliProgress,
    cancel: &CancelToken,
    json: bool,
) -> Result<()> {
    let game = LocalGameId::from_str(game).context("invalid game id")?;
    let mut desired = onera.database().active_installations(game).await?;
    for raw in disable {
        let id = InstallationId::from_str(raw).context("invalid disabled installation id")?;
        desired.retain(|candidate| *candidate != id);
    }
    for raw in enable {
        let id = InstallationId::from_str(raw).context("invalid enabled installation id")?;
        if !desired.contains(&id) {
            desired.push(id);
        }
    }
    let mut decisions = std::collections::BTreeMap::new();
    for raw in winners {
        let (target, winner) = raw
            .split_once('=')
            .context("winner must be ROOT:PATH=INSTALLATION_ID")?;
        let (root_key, path) = target
            .split_once(':')
            .context("winner target must be ROOT:PATH")?;
        let winner = InstallationId::from_str(winner).context("invalid winner installation id")?;
        decisions.insert(
            onera_core::plan::TargetLocation {
                root_key: root_key.to_owned(),
                path: onera_core::RelPath::normalize(path).context("invalid winner path")?,
            },
            winner,
        );
    }
    let prepared = onera
        .plan_state_with_decisions(game, desired, &decisions)
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&prepared.plan)?);
    } else {
        println!(
            "{} filesystem change(s), {} conflict(s)",
            prepared.plan.steps.len(),
            prepared.plan.conflicts.len()
        );
        for step in &prepared.plan.steps {
            println!("  {step:?}");
        }
        for conflict in &prepared.plan.conflicts {
            println!("  conflict {}: {:?}", conflict.target, conflict.providers);
        }
    }
    if !apply {
        return Ok(());
    }
    if !prepared.plan.is_ready() {
        anyhow::bail!("the desired state has unresolved cross-mod conflicts");
    }
    onera.apply_state(&prepared, progress, cancel).await?;
    if !json {
        println!("desired state applied");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn remove(
    onera: &Onera,
    game: &str,
    installation: &str,
    dry_run: bool,
    force_modified: bool,
    progress: &CliProgress,
    cancel: &CancelToken,
    json: bool,
) -> Result<()> {
    let game = LocalGameId::from_str(game).context("invalid game id")?;
    let installation = InstallationId::from_str(installation).context("invalid installation id")?;

    let report = if dry_run {
        onera.preview_removal(game, installation).await?
    } else {
        onera
            .remove(
                game,
                installation,
                if force_modified {
                    ModifiedFilePolicy::Force
                } else {
                    ModifiedFilePolicy::Ask
                },
                progress,
                cancel,
            )
            .await?
    };

    emit(
        json,
        &serde_json::json!({
            "deleted": report.deleted.len(),
            "restored": report.restored.len(),
            "kept_shared": report.kept_shared.len(),
            "already_missing": report.already_missing.len(),
            "externally_modified": report.externally_modified.len(),
            "directories_removed": report.directories_removed.len(),
            "dry_run": dry_run,
        }),
        || {
            format!(
                "{}{} deleted, {} restored, {} kept (shared), {} already gone, {} modified",
                if dry_run { "would be: " } else { "" },
                report.deleted.len(),
                report.restored.len(),
                report.kept_shared.len(),
                report.already_missing.len(),
                report.externally_modified.len()
            )
        },
    );
    Ok(())
}

async fn ownership(onera: &Onera, game: &str, root: &str, path: &str, json: bool) -> Result<()> {
    let target = onera_core::plan::TargetLocation {
        root_key: root.to_owned(),
        path: onera_core::RelPath::normalize(path).context("invalid path")?,
    };
    let stack = onera
        .ownership(
            LocalGameId::from_str(game).context("invalid game id")?,
            &target,
        )
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&stack)?);
        return Ok(());
    }
    if stack.is_empty() {
        println!("{target} is not managed by Onera");
        return Ok(());
    }
    println!("{target}, oldest provider first:");
    for (index, entry) in stack.entries().iter().enumerate() {
        let who = match entry.provider.installation_id() {
            Some(id) => format!("installation {id}"),
            None => "unmanaged original (backed up)".to_owned(),
        };
        println!(
            "  {index}. {who}  {}  {} bytes{}",
            entry.hash.prefix(12),
            entry.size,
            if index + 1 == stack.len() {
                "  <- deployed"
            } else {
                ""
            }
        );
    }
    Ok(())
}

async fn recover(onera: &Onera, rollback: bool, progress: &CliProgress, json: bool) -> Result<()> {
    let interrupted = onera.interrupted_operations().await?;
    if interrupted.is_empty() {
        // An activation can be interrupted before it journals anything, so the
        // records are finalized even when no operation needs recovering.
        let activations = onera.recover_profile_activations().await?;
        emit(json, &serde_json::to_value(&activations)?, || {
            if activations.is_empty() {
                "no interrupted operations".to_owned()
            } else {
                format!("finalized {} interrupted activation(s)", activations.len())
            }
        });
        return Ok(());
    }

    for item in &interrupted {
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "operation": item.operation.id.to_string(),
                    "state": item.operation.state.to_string(),
                    "recovery": format!("{:?}", item.recovery),
                    "committed": item.committed_files,
                    "staged": item.staged_files,
                }))?
            );
        } else {
            println!(
                "{}  state={}  committed={}  staged={}  recovery={:?}",
                item.operation.id,
                item.operation.state,
                item.committed_files,
                item.staged_files,
                item.recovery
            );
        }
        if rollback {
            onera.roll_back(item.operation.id, progress).await?;
            if !json {
                println!("  rolled back");
            }
        }
    }
    // Only after the journal half has been resolved: an activation whose
    // operation is still in flight is deliberately left alone.
    let activations = onera.recover_profile_activations().await?;
    if !json {
        for activation in &activations {
            println!(
                "activation of {} finalized as {:?}",
                activation.to_profile_id, activation.state
            );
        }
    }
    if !rollback && !interrupted.is_empty() && !json {
        println!("\npass --rollback to undo these, or resolve them in the desktop application");
    }
    Ok(())
}

/// `onera baseline …` — one application method per subcommand.
///
/// `--json` prints the payloads in `docs/frontend-contracts.md` verbatim, which
/// is what keeps the desktop and the CLI incapable of disagreeing about them.
async fn baseline(
    onera: &Onera,
    action: BaselineAction,
    progress: &CliProgress,
    cancel: &CancelToken,
    json: bool,
) -> Result<()> {
    match action {
        BaselineAction::Status { game } => {
            let report = onera
                .baseline_status(LocalGameId::from_str(&game).context("invalid game id")?)
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                match &report.baseline {
                    None => println!("no baseline has been captured for this installation"),
                    Some(baseline) => {
                        println!("baseline:   {}", baseline.id);
                        println!("source:     {}", source_label(baseline.source));
                        println!("captured:   {}", baseline.captured_at);
                        println!(
                            "contents:   {} files, {} bytes",
                            baseline.file_count, baseline.total_bytes
                        );
                    }
                }
                println!("freshness:  {}", freshness_label(&report.freshness));
                println!("active mods: {}", report.active_mod_count);
                if let Some(reason) = &report.capture_blocked_reason {
                    println!("capture blocked: {reason}");
                }
            }
            Ok(())
        }
        BaselineAction::Capture {
            game,
            verified,
            local_snapshot,
            dry_run,
        } => {
            let game = LocalGameId::from_str(&game).context("invalid game id")?;
            let source = local_snapshot.then_some(BaselineSource::LocalSnapshot);
            if dry_run {
                let preview = onera.plan_baseline_capture(game, source).await?;
                emit(json, &serde_json::to_value(&preview)?, || {
                    format!(
                        "would scan {} root(s), {} file(s), {} bytes as {}",
                        preview.roots.len(),
                        preview.estimated_files,
                        preview.estimated_bytes,
                        source_label(preview.source)
                    )
                });
                return Ok(());
            }
            let baseline = onera
                .capture_baseline(game, source, verified, progress, cancel)
                .await?;
            emit(json, &serde_json::to_value(&baseline)?, || {
                format!(
                    "captured {} as {}: {} files, {} bytes",
                    baseline.id,
                    source_label(baseline.source),
                    baseline.file_count,
                    baseline.total_bytes
                )
            });
            Ok(())
        }
        BaselineAction::Clean { game, apply } => {
            let game = LocalGameId::from_str(&game).context("invalid game id")?;
            if apply {
                let report = onera.apply_return_to_clean(game, progress, cancel).await?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!(
                        "restored {} file(s) from Onera backups",
                        report.restored.len()
                    );
                    print_clean_context(&report.needs_store_repair, &report.unknown_extras);
                    println!(
                        "{}",
                        if report.clean {
                            "clean: the installation matches its baseline"
                        } else {
                            "not clean: the differences above remain"
                        }
                    );
                }
                if !report.clean {
                    // A non-zero exit lets a script notice without parsing output.
                    std::process::exit(2);
                }
                return Ok(());
            }
            let preview = onera.plan_return_to_clean(game, progress, cancel).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&preview)?);
            } else {
                println!("{} filesystem step(s) planned", preview.plan.steps.len());
                println!(
                    "{} file(s) restorable from Onera backups",
                    preview.restorable.len()
                );
                print_clean_context(&preview.needs_store_repair, &preview.unknown_extras);
                println!("pass --apply to carry this out");
            }
            Ok(())
        }
        BaselineAction::Verify { game, quick } => {
            let game = LocalGameId::from_str(&game).context("invalid game id")?;
            let verification = onera.verify_baseline(game, quick, progress, cancel).await?;
            let baseline = onera
                .baseline_status(game)
                .await?
                .baseline
                .context("the baseline vanished while it was being verified")?;
            let clean = verification.is_clean(&baseline);
            if json {
                println!("{}", serde_json::to_string_pretty(&verification)?);
            } else {
                let counts = &verification.counts;
                println!("state:     {:?}", verification.state);
                println!("evidence:  {:?}", verification.evidence);
                println!(
                    "matching {} modified {} missing {} extra-managed {} extra-unknown {} \
                     unreadable {} special {}",
                    counts.matching,
                    counts.modified,
                    counts.missing,
                    counts.extra_managed,
                    counts.extra_unknown,
                    counts.unreadable,
                    counts.special
                );
                for finding in verification.findings.iter().filter(|finding| {
                    finding.classification
                        != onera_core::domain::baseline::FileClassification::Matching
                }) {
                    println!(
                        "  {:?} {}:{}",
                        finding.classification, finding.root_key, finding.path
                    );
                }
                println!(
                    "{}",
                    if clean {
                        "clean"
                    } else {
                        "not clean (a metadata-only scan is never clean)"
                    }
                );
            }
            if !clean {
                // A non-zero exit lets a script notice without parsing output.
                std::process::exit(2);
            }
            Ok(())
        }
    }
}

/// The two lists a return-to-clean reports rather than acts on.
fn print_clean_context(
    needs_store_repair: &[onera_app::StoreRepair],
    unknown_extras: &[onera_app::UnknownExtra],
) {
    if !needs_store_repair.is_empty() {
        println!(
            "{} file(s) need the store's own repair — Onera has no trusted copy:",
            needs_store_repair.len()
        );
        for repair in needs_store_repair {
            println!(
                "  {:?} {}:{}",
                repair.classification, repair.root_key, repair.path
            );
        }
    }
    if !unknown_extras.is_empty() {
        println!(
            "{} unknown extra file(s), which Onera never deletes:",
            unknown_extras.len()
        );
        for extra in unknown_extras {
            println!("  {}:{}", extra.root_key, extra.path);
        }
    }
}

const fn source_label(source: BaselineSource) -> &'static str {
    match source {
        BaselineSource::StoreVerifiedCapture => "store-verified capture",
        BaselineSource::LocalSnapshot => "local snapshot (not store-verified)",
        BaselineSource::StoreManifest => "store manifest",
    }
}

fn freshness_label(freshness: &BaselineFreshness) -> String {
    match freshness {
        BaselineFreshness::None => "none — nothing captured yet".to_owned(),
        BaselineFreshness::Fresh => "fresh".to_owned(),
        BaselineFreshness::Stale { .. } => {
            "stale — the store's build identity changed; verify files and recapture".to_owned()
        }
        BaselineFreshness::Unknown { reason } => format!("unknown — {reason}"),
    }
}

/// Print either JSON or a human-readable line.
fn emit(json: bool, value: &serde_json::Value, text: impl FnOnce() -> String) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    } else {
        println!("{}", text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_messaging_manifest_uses_the_stable_extension_identity() {
        let manifest = native_messaging_manifest(std::path::Path::new("/opt/onera/onera-nmhost"));
        assert_eq!(manifest["name"], "com.onera.host");
        assert_eq!(manifest["path"], "/opt/onera/onera-nmhost");
        assert_eq!(
            manifest["allowed_origins"][0],
            format!("chrome-extension://{EXTENSION_ID}/")
        );
    }

    #[test]
    fn relative_host_paths_are_made_absolute() {
        assert!(absolute_path("onera-nmhost".into()).unwrap().is_absolute());
    }

    #[test]
    fn packaged_manifest_matches_the_extension_identity() {
        let packaged: serde_json::Value =
            serde_json::from_str(include_str!("../../../packaging/com.onera.host.json")).unwrap();
        let extension: serde_json::Value =
            serde_json::from_str(include_str!("../../../extension/manifest.json")).unwrap();
        assert_eq!(
            packaged["allowed_origins"][0],
            format!("chrome-extension://{EXTENSION_ID}/")
        );
        assert!(extension["key"].as_str().is_some_and(|key| !key.is_empty()));
    }

    #[test]
    fn profile_commands_parse() {
        let id = "00000000-0000-0000-0000-000000000000".to_owned();
        for args in [
            vec!["onera", "profiles", "list", "--game", &id],
            vec![
                "onera", "profiles", "create", "--game", &id, "--name", "Survival",
            ],
            vec!["onera", "profiles", "show", &id],
            vec!["onera", "profiles", "enable", &id],
            vec!["onera", "profiles", "disable", &id],
            vec!["onera", "profiles", "pin", &id, "--unpin"],
            vec!["onera", "profiles", "reorder", &id, "--priority", "-10"],
            vec!["onera", "profiles", "plan-activate", "--profile", &id],
            vec!["onera", "profiles", "activate", "--profile", &id],
            vec![
                "onera",
                "profiles",
                "activate",
                "--profile",
                &id,
                "--expect",
                "b3:0123",
            ],
        ] {
            assert!(Cli::try_parse_from(args.clone()).is_ok(), "{args:?}");
        }
        // The profile is named, never positional: activating the wrong game's
        // profile because two ids swapped places is not a mistake worth
        // allowing.
        assert!(Cli::try_parse_from(["onera", "profiles", "activate", &id]).is_err());
    }

    #[test]
    fn dependency_commands_parse() {
        let id = "00000000-0000-0000-0000-000000000000".to_owned();
        for args in [
            vec!["onera", "deps", "check", "--profile", &id],
            vec![
                "onera",
                "deps",
                "check",
                "--profile",
                &id,
                "--preview-add",
                "mod-1:9001",
                "--preview-disable",
                &id,
            ],
            vec!["onera", "deps", "show", "--mod", &id, "--file", "9001"],
            vec![
                "onera",
                "deps",
                "ignore",
                "--member",
                &id,
                "--group",
                &id,
                "--fingerprint",
                "b3:definition",
                "--reason",
                "I install it by hand",
            ],
            vec![
                "onera",
                "deps",
                "ignore",
                "--member",
                &id,
                "--group",
                &id,
                "--fingerprint",
                "b3:definition",
                "--clear",
            ],
            vec!["onera", "deps", "apply-plan", "--profile", &id],
            vec!["onera", "deps", "plan-updates", "--profile", &id],
            vec![
                "onera",
                "deps",
                "apply-updates",
                "--profile",
                &id,
                "--expect",
                "b3:update",
            ],
        ] {
            assert!(Cli::try_parse_from(args.clone()).is_ok(), "{args:?}");
        }

        // Ignoring is always an attributable decision, so a reason is required
        // unless the decision is being withdrawn.
        assert!(Cli::try_parse_from([
            "onera",
            "deps",
            "ignore",
            "--member",
            &id,
            "--group",
            &id,
            "--fingerprint",
            "b3:definition",
        ])
        .is_err());
        // Identifiers are named, never positional.
        assert!(Cli::try_parse_from(["onera", "deps", "check", &id]).is_err());
        assert!(Cli::try_parse_from(["onera", "deps", "show", "--mod", &id]).is_err());
    }

    #[test]
    fn dependency_help_lists_the_documented_subcommands() {
        let rendered = Cli::try_parse_from(["onera", "deps", "--help"])
            .expect_err("--help exits through the error path")
            .to_string();
        for command in [
            "check",
            "show",
            "ignore",
            "apply-plan",
            "plan-updates",
            "apply-updates",
        ] {
            assert!(
                rendered.contains(command),
                "missing {command} in:\n{rendered}"
            );
        }
    }

    #[test]
    fn a_resolution_prints_its_contract_payload() {
        use onera_core::domain::dependency::{
            DependencyHealth, MemberHealth, ResolutionEvidence, ResolutionOutcome, ResolutionResult,
        };

        let result = ResolutionResult {
            outcome: ResolutionOutcome::Unknown {
                reason: "the provider could not be reached".to_owned(),
            },
            health: vec![MemberHealth {
                profile_member_id: ProfileMemberId::new(),
                health: DependencyHealth::Unknown,
                unsatisfied: vec![],
            }],
            evidence: ResolutionEvidence::default(),
        };
        let json = serde_json::to_value(&result).unwrap();
        for field in ["outcome", "health", "evidence"] {
            assert!(json.get(field).is_some(), "missing {field}");
        }
        assert_eq!(json["outcome"]["kind"], serde_json::json!("unknown"));
        assert_eq!(json["health"][0]["health"], serde_json::json!("unknown"));
        for field in [
            "fresh",
            "cached",
            "stale",
            "unavailable",
            "unsupported",
            "unknown_dlc",
        ] {
            assert!(json["evidence"].get(field).is_some(), "missing {field}");
        }
    }

    #[test]
    fn every_outcome_names_itself_and_only_one_reads_as_compatible() {
        use onera_core::domain::dependency::{
            DependencySource, ResolutionOutcome as O, UnsatisfiedRequirement,
        };
        use onera_core::ids::{DependencyGroupId, ProviderId, ProviderModId};

        let requirement = UnsatisfiedRequirement {
            source: DependencySource {
                provider: ProviderId::new("nexus"),
                game_slug: "cyberpunk2077".to_owned(),
                provider_mod_id: ProviderModId::new("107"),
                provider_file_id: None,
                provider_version_id: None,
            },
            group_id: DependencyGroupId::new(),
            label: None,
            explanation: "no available candidate targets this game".to_owned(),
        };
        let cases = [
            (O::Compatible, "compatible"),
            (O::InstallMissing { install: vec![] }, "install-missing"),
            (
                O::UpdateSet {
                    select: vec![],
                    install: vec![],
                },
                "update-set",
            ),
            (O::DisableSet { disable: vec![] }, "disable-set"),
            (
                O::Unsatisfied {
                    requirements: vec![requirement],
                },
                "unsatisfied",
            ),
            (
                O::Unknown {
                    reason: "offline".to_owned(),
                },
                "unknown",
            ),
        ];
        for (outcome, expected) in cases {
            let line = resolution_headline(&outcome);
            assert!(line.starts_with(expected), "{expected} vs {line}");
            if expected != "compatible" {
                assert!(!line.starts_with("compatible"), "{line}");
            }
        }
    }

    #[test]
    fn an_activation_preview_prints_its_contract_payload() {
        use onera_core::domain::baseline::BaselineFreshness;
        use onera_core::domain::dependency::{
            ResolutionEvidence, ResolutionOutcome, ResolutionResult,
        };
        use onera_core::domain::profile::ProfileActivationPreview;
        use onera_core::domain::reconcile::{reconcile, DesiredGameState};
        use onera_core::ids::LocalGameId;

        let preview = ProfileActivationPreview::assemble(
            None,
            ProfileId::new(),
            reconcile(
                DesiredGameState::new(LocalGameId::new(), vec![]),
                &std::collections::BTreeMap::new(),
                &[],
            ),
            vec![],
            ResolutionResult {
                outcome: ResolutionOutcome::Compatible,
                health: vec![],
                evidence: ResolutionEvidence::default(),
            },
            BaselineFreshness::Fresh,
            &[],
        );
        let json = serde_json::to_value(&preview).unwrap();
        // The fields `docs/frontend-contracts.md` promises, by name.
        for field in [
            "from_profile_id",
            "to_profile_id",
            "plan",
            "downloads",
            "dependency",
            "baseline_freshness",
            "bytes_to_write",
            "ready",
            "blockers",
        ] {
            assert!(json.get(field).is_some(), "missing {field}");
        }
        assert_eq!(json["ready"], serde_json::json!(true));
    }
}
