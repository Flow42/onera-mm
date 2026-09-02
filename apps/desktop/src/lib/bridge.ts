/**
 * The Tauri command bridge.
 *
 * Every call into the Rust core goes through here. Two reasons for the
 * indirection rather than importing `invoke` at each call site:
 *
 * 1. it is the single place where an untrusted result is narrowed into a typed
 *    one, so a backend change surfaces as one type error rather than a runtime
 *    surprise scattered across components;
 * 2. it can be replaced wholesale in tests and in the Playwright suite, which is
 *    how the frontend is exercised without a compiled desktop binary.
 *
 * No filesystem, installation or conflict logic lives on this side of the
 * bridge. Commands are named after application operations, not after UI events.
 */

import type {
  AccountInfo,
  BaselineCapturePreview,
  BaselineSource,
  BaselineStatus,
  BaselineVerification,
  CleanRestorePreview,
  CleanRestoreReport,
  DownloadJob,
  DownloadOutcome,
  GameBaseline,
  DiscoveredGame,
  InstallPlanView,
  InstalledMod,
  InboxRequest,
  InterruptedOperation,
  LocalGame,
  ModDetails,
  ProviderStackView,
  Profile,
  ProfileActivation,
  ProfileActivationPreview,
  ProfileMember,
  RemovalPreview,
  ResolutionResult,
  StartupStatus,
  VerifyReport,
} from './types';

/** Shape of the injected bridge, so tests can supply their own. */
export interface CommandBridge {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>;
}

let bridge: CommandBridge | null = null;

/**
 * Replace the bridge. Called by tests and by the Playwright harness.
 *
 * @param replacement - The bridge to use, or `null` to fall back to Tauri.
 */
export function setBridge(replacement: CommandBridge | null): void {
  bridge = replacement;
}

/** Resolve the bridge, defaulting to the real Tauri one. */
async function resolve(): Promise<CommandBridge> {
  if (bridge !== null) {
    return bridge;
  }
  // The Playwright suite injects a bridge before the app loads, so the real
  // views can be driven without a compiled desktop binary.
  const injected = (globalThis as { __ONERA_TEST_BRIDGE__?: CommandBridge }).__ONERA_TEST_BRIDGE__;
  if (injected !== undefined) {
    bridge = injected;
    return bridge;
  }
  const [{ invoke }, { listen }] = await Promise.all([
    import('@tauri-apps/api/core'),
    import('@tauri-apps/api/event'),
  ]);
  bridge = {
    invoke: <T>(command: string, args?: Record<string, unknown>) => invoke<T>(command, args),
    listen: async <T>(event: string, handler: (payload: T) => void) => {
      const unlisten = await listen<T>(event, (e) => handler(e.payload));
      return unlisten;
    },
  };
  return bridge;
}

/**
 * A failure returned by the Rust core, already redacted.
 *
 * `code` is stable and safe to branch on; `message` is for display only.
 */
export class BridgeError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = 'BridgeError';
    this.code = code;
  }
}

/** Call one command, normalising failures into {@link BridgeError}. */
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const active = await resolve();
  try {
    return await active.invoke<T>(command, args);
  } catch (error) {
    throw normaliseError(error);
  }
}

/**
 * Turn whatever the bridge threw into a typed error.
 *
 * Exported because it is worth testing directly: the backend can throw a
 * string, a structured object or an `Error`, and the UI must render all three.
 *
 * @param error - The thrown value.
 * @returns A {@link BridgeError}.
 */
export function normaliseError(error: unknown): BridgeError {
  if (error instanceof BridgeError) {
    return error;
  }
  if (typeof error === 'string') {
    return new BridgeError('internal', error);
  }
  if (error !== null && typeof error === 'object') {
    const { code, message } = error as { code?: unknown; message?: unknown };
    return new BridgeError(
      typeof code === 'string' ? code : 'internal',
      typeof message === 'string' ? message : 'Onera reported an error.',
    );
  }
  return new BridgeError('internal', 'Onera reported an error.');
}

/** Subscribe to streamed progress events for long-running operations. */
export async function onProgress(handler: (event: ProgressEvent) => void): Promise<() => void> {
  const active = await resolve();
  return active.listen<ProgressEvent>('onera://progress', handler);
}

/** A progress event, mirroring `onera_core::progress::ProgressEvent`. */
export type ProgressEvent =
  | { type: 'started'; stage: string; total: number | null }
  | {
      type: 'advanced';
      stage: string;
      completed: number;
      total: number | null;
      detail: string | null;
    }
  | { type: 'warning'; message: string }
  | { type: 'finished'; stage: string; success: boolean };

export const commands = {
  /* onboarding */
  startupStatus: () => call<StartupStatus>('startup_status'),
  isAuthenticated: () => call<boolean>('is_authenticated'),
  setApiKey: (key: string) => call<AccountInfo>('set_api_key', { key }),
  forgetApiKey: () => call<void>('forget_api_key'),
  account: () => call<AccountInfo>('account'),

  /* games */
  discoverGames: () => call<DiscoveredGame[]>('discover_games'),
  confirmGame: (game: DiscoveredGame) => call<string>('confirm_game', { game }),
  addManualGame: (path: string) => call<DiscoveredGame>('add_manual_game', { path }),
  localGames: () => call<LocalGame[]>('local_games'),

  /* mods */
  fetchMod: (gameDomain: string, modId: string) =>
    call<ModDetails>('fetch_mod', { gameDomain, modId }),
  installedMods: (gameId: string) => call<InstalledMod[]>('installed_mods', { gameId }),
  checkUpdates: (gameId: string) => call<InstalledMod[]>('check_updates', { gameId }),
  inboxRequests: () => call<InboxRequest[]>('inbox_requests'),
  dismissInboxRequest: (requestId: string) => call<void>('dismiss_inbox_request', { requestId }),
  completeInboxRequest: (requestId: string) => call<void>('complete_inbox_request', { requestId }),

  /* downloads */
  downloads: () => call<DownloadJob[]>('downloads'),
  downloadFile: (gameDomain: string, modId: string, fileId: string) =>
    call<DownloadOutcome>('download_file', { gameDomain, modId, fileId }),
  resumeDownloads: () => call<void>('resume_downloads'),

  /* installation */
  prepareInstall: (args: { gameId: string; gameDomain: string; modId: string; fileId: string }) =>
    call<InstallPlanView>('prepare_install', args),
  decide: (operationId: string, target: string, choice: string, scope: string) =>
    call<InstallPlanView>('decide', { operationId, target, choice, scope }),
  applyPlan: (operationId: string) => call<{ written: number }>('apply_plan', { operationId }),
  cancelOperation: (operationId: string) => call<void>('cancel_operation', { operationId }),

  /* verification, removal, history */
  verify: (gameId: string, installationId: string) =>
    call<VerifyReport>('verify', { gameId, installationId }),
  previewRemoval: (gameId: string, installationId: string) =>
    call<RemovalPreview>('preview_removal', { gameId, installationId }),
  remove: (gameId: string, installationId: string, force: boolean) =>
    call<RemovalPreview>('remove_mod', { gameId, installationId, force }),
  ownership: (gameId: string, rootKey: string, path: string) =>
    call<ProviderStackView>('ownership', { gameId, rootKey, path }),

  /* baseline */
  baselineStatus: (gameId: string) => call<BaselineStatus>('baseline_status', { gameId }),
  planBaselineCapture: (gameId: string, source?: BaselineSource) =>
    call<BaselineCapturePreview>('plan_baseline_capture', { gameId, source: source ?? null }),
  captureBaseline: (gameId: string, storeVerificationConfirmed: boolean, source?: BaselineSource) =>
    call<GameBaseline>('capture_baseline', {
      gameId,
      source: source ?? null,
      storeVerificationConfirmed,
    }),
  verifyBaseline: (gameId: string, quick: boolean) =>
    call<BaselineVerification>('verify_baseline', { gameId, quick }),
  planReturnToClean: (gameId: string) =>
    call<CleanRestorePreview>('plan_return_to_clean', { gameId }),
  applyReturnToClean: (gameId: string) =>
    call<CleanRestoreReport>('apply_return_to_clean', { gameId }),

  /* profiles (desired state only until activateProfile) */
  profiles: (gameId: string) => call<Profile[]>('profiles', { gameId }),
  profileMembers: (profileId: string) => call<ProfileMember[]>('profile_members', { profileId }),
  createProfile: (gameId: string, name: string, description?: string, copyFromProfileId?: string) =>
    call<Profile>('create_profile', {
      gameId,
      name,
      description: description ?? null,
      copyFromProfileId: copyFromProfileId ?? null,
    }),
  renameProfile: (profileId: string, name: string) =>
    call<Profile>('rename_profile', { profileId, name }),
  deleteProfile: (profileId: string) => call<void>('delete_profile', { profileId }),
  addProfileMember: (profileId: string, modId: string, providerFileId?: string) =>
    call<ProfileMember>('add_profile_member', {
      profileId,
      modId,
      providerFileId: providerFileId ?? null,
    }),
  removeProfileMember: (memberId: string) => call<void>('remove_profile_member', { memberId }),
  setMemberState: (memberId: string, desired: 'enabled' | 'disabled') =>
    call<ProfileMember>('set_member_state', { memberId, desired }),
  setMemberPin: (memberId: string, pinned: boolean, reason?: string) =>
    call<ProfileMember>('set_member_pin', { memberId, pinned, reason: reason ?? null }),
  reorderProfileMember: (memberId: string, priority: number) =>
    call<ProfileMember>('reorder_profile_member', { memberId, priority }),
  resolveDependencies: (profileId: string) =>
    call<ResolutionResult>('resolve_dependencies', { profileId }),
  planProfileActivation: (profileId: string) =>
    call<ProfileActivationPreview>('plan_profile_activation', { profileId }),
  activateProfile: (profileId: string, expectedFingerprint: string) =>
    call<ProfileActivation>('activate_profile', { profileId, expectedFingerprint }),

  /* recovery and diagnostics */
  interruptedOperations: () => call<InterruptedOperation[]>('interrupted_operations'),
  rollBack: (operationId: string) => call<void>('roll_back', { operationId }),
  diagnostics: () => call<Record<string, string>>('diagnostics'),
};
