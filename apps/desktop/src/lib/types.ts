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
