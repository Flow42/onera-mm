/**
 * Types mirroring the Rust core's serialised shapes.
 *
 * Kept deliberately narrow: the frontend only declares the fields it renders,
 * so adding a field on the Rust side is never a breaking change here.
 */

export interface AccountInfo {
  provider_user_id: string;
  username: string;
  premium: boolean | null;
  email: string | null;
}

export interface StartupStatus {
  authenticated: boolean;
  recovery_required: boolean;
  inbox_count: number;
  expired_plans: number;
}

export interface DiscoveredGame {
  adapter_id: string;
  provider_slug: string | null;
  name: string;
  install_root: string;
  compat_prefix: string | null;
  user_data_roots: string[];
  source: 'steam_native' | 'steam_flatpak' | 'manual';
  validation: { valid: boolean; reported_version: string | null; findings: string[] };
}

export interface LocalGame {
  id: string;
  adapter_id: string;
  install_root: string;
  confirmed: boolean;
}

export interface ProviderFileView {
  id: string;
  name: string;
  category: string;
  size: number | null;
  is_primary: boolean;
}

export interface ModDetails {
  mod_id: string;
  name: string;
  author: string | null;
  needs_file_selection: boolean;
  files: ProviderFileView[];
}

/** One file in a dry-run plan. */
export interface PlannedFileView {
  source: string;
  target: string;
  classification: Classification;
  action: string;
  existing_hash: string | null;
  notes: string[];
  decision: string | null;
}

export type Classification =
  | 'create'
  | 'identical'
  | 'replace_previous_release'
  | 'conflict_with_other_mod'
  | 'unmanaged_existing'
  | 'externally_modified'
  | 'invalid_target'
  | 'skipped_by_rule';

export interface InstallPlanView {
  operation_id: string;
  installation_id: string;
  mod_name: string;
  layout_rationale: string;
  ignored: number;
  rejected: { raw_path: string; reason: string }[];
  ready: boolean;
  bytes_to_write: number;
  files: PlannedFileView[];
}

export interface InstalledMod {
  installation_id: string;
  mod_id: string;
  name: string;
  version: string;
  installed_at: string;
  update_available: boolean;
  latest_version: string | null;
}

export interface InboxRequest {
  id: string;
  kind: 'add_mod' | 'download' | 'download_and_install';
  provider: string;
  game_slug: string;
  provider_mod_id: string;
  provider_file_id: string | null;
  state: 'queued' | 'waiting_for_user' | 'failed';
  error: string | null;
  created_at: string;
  updated_at: string;
}

export interface DownloadJob {
  id: string;
  provider: string;
  game_slug: string;
  provider_mod_id: string;
  provider_file_id: string;
  filename: string;
  expected_size: number | null;
  bytes_downloaded: number;
  state: 'queued' | 'running' | 'paused' | 'complete' | 'failed' | 'cancelled';
  attempts: number;
  error: string | null;
  archive_id: string | null;
}

export interface DownloadOutcome {
  archive_id: string;
  hash: string;
  bytes: number;
  deduplicated: boolean;
}

export interface VerifyReport {
  files: {
    target: string;
    status: 'ok' | 'modified' | 'missing' | 'unreadable';
    expected: string;
    actual: string | null;
  }[];
}

export interface RemovalPreview {
  deleted: string[];
  restored: string[];
  kept_shared: string[];
  already_missing: string[];
  externally_modified: string[];
  directories_removed: string[];
}

export interface ProviderStackView {
  entries: {
    kind: 'installation' | 'unmanaged_backup';
    installation_id: string | null;
    mod_name: string | null;
    hash: string;
    size: number;
  }[];
}

export interface InterruptedOperation {
  operation_id: string;
  kind: string;
  state: string;
  recovery: string;
  committed_files: number;
  staged_files: number;
  created_at: string;
}

// ---------------------------------------------------------------------------
// Baseline
// ---------------------------------------------------------------------------

/** Where a baseline's authority comes from. */
export type BaselineSource = 'store_verified_capture' | 'local_snapshot' | 'store_manifest';

/** Lifecycle of a captured baseline. */
export type BaselineStatusKind = 'capturing' | 'current' | 'superseded' | 'failed';

/** Best-effort build identity read from the store's own local metadata. */
export interface StoreBuildIdentity {
  store: 'steam' | 'manual';
  app_id: string | null;
  build_id: string | null;
  branch: string | null;
  depots: { depot_id: string; manifest_id: string }[];
  manifest_path: string | null;
  observed_at: string;
}

/**
 * Whether the current baseline still describes the installed build.
 *
 * `unknown` is a state of its own and must never be rendered as `fresh`.
 */
export type BaselineFreshness =
  | { kind: 'none' }
  | { kind: 'fresh' }
  | { kind: 'stale'; captured: StoreBuildIdentity; observed: StoreBuildIdentity }
  | { kind: 'unknown'; reason: string };

export interface GameBaseline {
  id: string;
  local_game_id: string;
  source: BaselineSource;
  build_identity: StoreBuildIdentity | null;
  adapter_id: string;
  reported_version: string | null;
  status: BaselineStatusKind;
  captured_at: string;
  scope_fingerprint: string;
  file_count: number;
  total_bytes: number;
}

export interface BaselineStatus {
  baseline: GameBaseline | null;
  freshness: BaselineFreshness;
  observed_build_identity: StoreBuildIdentity | null;
  active_mod_count: number;
  capture_blocked_reason: string | null;
}

export interface BaselineExclusionView {
  root_key: string | null;
  pattern: Record<string, unknown> & { kind: string };
  reason: string;
  note: string | null;
}

export interface BaselineCapturePreview {
  roots: { key: string; kind: string; path: string }[];
  exclusions: BaselineExclusionView[];
  estimated_files: number;
  estimated_bytes: number;
  source: BaselineSource;
  requires_store_verification: boolean;
  capture_blocked_reason: string | null;
}

/** How one path compares with the baseline. */
export type FileClassification =
  | 'matching'
  | 'modified'
  | 'missing'
  | 'extra_managed'
  | 'extra_unknown'
  | 'unreadable'
  | 'special_file';

export interface BaselineFinding {
  root_key: string;
  path: string;
  classification: FileClassification;
  expected: string | null;
  observed: string | null;
  detail: string | null;
}

export interface FindingCounts {
  matching: number;
  modified: number;
  missing: number;
  extra_managed: number;
  extra_unknown: number;
  unreadable: number;
  special: number;
}

export interface BaselineVerification {
  baseline_id: string;
  scan_run_id: string;
  state: 'running' | 'completed' | 'cancelled' | 'failed';
  evidence: 'content_hashed' | 'metadata_only';
  scope_fingerprint: string;
  findings: BaselineFinding[];
  counts: FindingCounts;
  verified_at: string;
}

export interface RestorableFile {
  root_key: string;
  path: string;
  from: 'backup';
}

export interface StoreRepair {
  root_key: string;
  path: string;
  classification: FileClassification;
}

export interface UnknownExtra {
  root_key: string;
  path: string;
}

export interface CleanRestorePreview {
  plan: { steps: unknown[] };
  restorable: RestorableFile[];
  needs_store_repair: StoreRepair[];
  unknown_extras: UnknownExtra[];
}

export interface CleanRestoreReport {
  plan: { steps: unknown[] };
  restored: RestorableFile[];
  needs_store_repair: StoreRepair[];
  unknown_extras: UnknownExtra[];
  verification: BaselineVerification;
  clean: boolean;
}

// ---------------------------------------------------------------------------
// Profiles and activation
// ---------------------------------------------------------------------------

/** A reusable desired mod set belonging to one concrete game installation. */
export interface Profile {
  id: string;
  local_game_id: string;
  name: string;
  description: string | null;
  is_active: boolean;
  created_at: string;
  updated_at: string;
}

export interface MemberSelection {
  provider: string;
  provider_mod_id: string;
  provider_file_id: string | null;
  provider_version_id: string | null;
  provider_file_group_id: string | null;
}

export type MemberPin =
  { kind: 'unpinned' } | { kind: 'pinned'; pinned_at: string; reason: string | null };

/** One row in a profile's signed-priority member list. */
export interface ProfileMember {
  id: string;
  profile_id: string;
  mod_id: string;
  selection: MemberSelection;
  installation_id: string | null;
  desired: 'enabled' | 'disabled';
  pin: MemberPin;
  priority: number;
  added_at: string;
}

export interface DependencyProblem {
  source: {
    provider: string;
    game_slug: string;
    provider_mod_id: string;
    provider_file_id: string | null;
    provider_version_id: string | null;
  };
  group_id: string;
  label: string;
  explanation: string;
}

export type DependencyHealthKind =
  'satisfied' | 'unsatisfied' | 'ignored' | 'not_applicable' | 'unknown';

export interface MemberDependencyHealth {
  profile_member_id: string;
  health: DependencyHealthKind;
  unsatisfied: DependencyProblem[];
}

export type DependencyOutcome =
  | { kind: 'compatible' }
  | { kind: 'unsatisfied' }
  | { kind: 'unknown'; reason?: string }
  | {
      kind: 'install_missing' | 'update_set' | 'disable_set';
      select?: Array<Record<string, string | null>>;
      install?: Array<Record<string, string | null>>;
      disable?: string[];
    };

export interface DependencyEvidence {
  fresh: number;
  cached: number;
  stale: number;
  unavailable: number;
  unsupported: number;
  unknown_dlc: number;
}

export interface ResolutionResult {
  outcome: DependencyOutcome;
  health: MemberDependencyHealth[];
  evidence: DependencyEvidence;
}

/** Adapter-root-relative path. Blocker targets are already formatted strings. */
export type MutationTarget = string | { root_key: string; path: string };

/**
 * The reconciler currently serialises write/delete steps. Keeping `kind` open
 * lets the view safely label later semantic step kinds instead of dropping
 * them or pretending that an unrecognised change is harmless.
 */
export interface MutationStep {
  kind: string;
  target: MutationTarget;
  provider?: {
    provider: {
      kind: string;
      installation_id?: string;
      backup_id?: string;
    };
    hash: string;
    size: number;
  };
  expected_previous?: string | null;
}

export interface MutationConflict {
  target: MutationTarget;
  providers: string[];
}

export interface MutationPlan {
  desired?: { local_game_id: string; installations: string[] };
  final_stacks?: unknown[];
  steps: MutationStep[];
  expected_files?: unknown[];
  conflicts: MutationConflict[];
  conflict_decisions?: unknown[];
}

export interface ActivationDownload {
  member_id: string;
  name: string;
  bytes: number | null;
}

export interface ActivationBlocker {
  kind: string;
  target?: string;
  member_id?: string;
  detail?: string;
}

export interface ProfileActivationPreview {
  from_profile_id: string | null;
  to_profile_id: string;
  plan: MutationPlan;
  downloads: ActivationDownload[];
  dependency: ResolutionResult;
  baseline_freshness: BaselineFreshness;
  bytes_to_write: number;
  ready: boolean;
  blockers: ActivationBlocker[];
  fingerprint: string;
}

export type ProfileActivationState =
  'preparing' | 'applying' | 'applied' | 'rolled_back' | 'failed';

export interface ProfileActivation {
  from_profile_id: string | null;
  to_profile_id: string;
  operation_id: string | null;
  state: ProfileActivationState;
  started_at: string;
  finished_at: string | null;
  error: string | null;
}
