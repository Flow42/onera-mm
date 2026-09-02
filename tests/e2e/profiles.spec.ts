import { expect, test, type Page } from '@playwright/test';

const GAME = 'game-1';

const game = {
  id: GAME,
  adapter_id: 'cyberpunk2077',
  install_root: '/games/Cyberpunk 2077',
  confirmed: true,
};

const profile = (id: string, name: string, active = false) => ({
  id,
  local_game_id: GAME,
  name,
  description: name === 'Default' ? 'Imported active mods' : null,
  is_active: active,
  created_at: '2026-08-01T12:00:00Z',
  updated_at: '2026-09-01T18:22:00Z',
});

const member = (over: Record<string, unknown> = {}) => ({
  id: 'member-1',
  profile_id: 'profile-default',
  mod_id: 'mod-cet',
  selection: {
    provider: 'nexus',
    provider_mod_id: '107',
    provider_file_id: '9001',
    provider_version_id: 'v-9001',
    provider_file_group_id: 'g-107',
  },
  installation_id: 'installation-1',
  desired: 'enabled',
  pin: { kind: 'unpinned' },
  priority: 10,
  added_at: '2026-08-01T12:30:00Z',
  ...over,
});

const identity = (build: string) => ({
  store: 'steam',
  app_id: '1091500',
  build_id: build,
  branch: null,
  depots: [],
  manifest_path: '/games/steamapps/appmanifest_1091500.acf',
  observed_at: '2026-09-02T09:00:00Z',
});

const dependency = (over: Record<string, unknown> = {}) => ({
  outcome: { kind: 'compatible' },
  health: [{ profile_member_id: 'member-1', health: 'satisfied', unsatisfied: [] }],
  evidence: {
    fresh: 1,
    cached: 0,
    stale: 0,
    unavailable: 0,
    unsupported: 0,
    unknown_dlc: 0,
  },
  ...over,
});

const preview = (over: Record<string, unknown> = {}) => ({
  from_profile_id: 'profile-default',
  to_profile_id: 'profile-quiet',
  plan: { steps: [], conflicts: [] },
  downloads: [],
  dependency: dependency(),
  baseline_freshness: { kind: 'fresh' },
  bytes_to_write: 0,
  ready: true,
  blockers: [],
  ...over,
});

interface MockConfig {
  games?: unknown[];
  profiles?: unknown[];
  members?: Record<string, unknown[]>;
  dependency?: unknown;
  preview?: unknown;
  activation?: unknown;
  errors?: Record<string, { code: string; message: string }>;
  profilesDelay?: number;
  interrupted?: unknown[];
}

/** A stateful in-browser implementation of the documented Tauri commands. */
async function stubProfiles(page: Page, supplied: MockConfig = {}) {
  const config = {
    games: [game],
    profiles: [profile('profile-default', 'Default', true), profile('profile-quiet', 'Quiet')],
    members: {
      'profile-default': [member()],
      'profile-quiet': [],
    },
    dependency: dependency(),
    preview: preview(),
    activation: {
      from_profile_id: 'profile-default',
      to_profile_id: 'profile-quiet',
      operation_id: 'operation-1',
      state: 'applied',
      started_at: '2026-09-02T10:00:00Z',
      finished_at: '2026-09-02T10:01:00Z',
      error: null,
    },
    errors: {},
    profilesDelay: 0,
    interrupted: [],
    ...supplied,
  };

  await page.addInitScript((settings) => {
    const state = structuredClone(settings) as typeof settings;
    const calls: { command: string; args?: Record<string, unknown> }[] = [];
    const listeners: Array<(payload: unknown) => void> = [];
    let sequence = 10;

    const rows = () => state.profiles as Array<Record<string, unknown>>;
    const allMembers = () => state.members as Record<string, Array<Record<string, unknown>>>;
    const findMember = (memberId: unknown) => {
      for (const list of Object.values(allMembers())) {
        const found = list.find((candidate) => candidate.id === memberId);
        if (found !== undefined) return found;
      }
      throw { code: 'not_found', message: 'member not found' };
    };
    const emit = (payload: unknown) => listeners.forEach((listener) => listener(payload));

    // @ts-expect-error - test-only call log exposed for assertions.
    window.__ONERA_CALLS__ = calls;
    // @ts-expect-error - injected for the desktop bridge to discover.
    window.__ONERA_TEST_BRIDGE__ = {
      invoke: async (command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        const configuredError = (state.errors as Record<string, unknown>)[command];
        if (configuredError !== undefined) throw configuredError;
        switch (command) {
          case 'local_games':
            return state.games;
          case 'profiles':
            if (state.profilesDelay > 0) {
              await new Promise((resolve) => setTimeout(resolve, state.profilesDelay));
            }
            return rows().filter((candidate) => candidate.local_game_id === args?.gameId);
          case 'profile_members':
            return allMembers()[String(args?.profileId)] ?? [];
          case 'resolve_dependencies':
            return state.dependency;
          case 'create_profile': {
            const created = {
              id: `profile-${sequence++}`,
              local_game_id: args?.gameId,
              name: args?.name,
              description: args?.description ?? null,
              is_active: false,
              created_at: '2026-09-02T10:00:00Z',
              updated_at: '2026-09-02T10:00:00Z',
            };
            rows().push(created);
            const source = allMembers()[String(args?.copyFromProfileId)] ?? [];
            allMembers()[String(created.id)] = source.map((candidate) => ({
              ...structuredClone(candidate),
              id: `member-${sequence++}`,
              profile_id: created.id,
            }));
            return created;
          }
          case 'rename_profile': {
            const selected = rows().find((candidate) => candidate.id === args?.profileId);
            if (selected === undefined) throw { code: 'not_found', message: 'profile not found' };
            selected.name = args?.name;
            return selected;
          }
          case 'delete_profile': {
            const index = rows().findIndex((candidate) => candidate.id === args?.profileId);
            if (index < 0) throw { code: 'not_found', message: 'profile not found' };
            if (rows()[index].is_active) {
              throw { code: 'conflict', message: 'cannot delete active profile' };
            }
            rows().splice(index, 1);
            return undefined;
          }
          case 'add_profile_member': {
            const created = {
              id: `member-${sequence++}`,
              profile_id: args?.profileId,
              mod_id: args?.modId,
              selection: {
                provider: 'nexus',
                provider_mod_id: args?.modId,
                provider_file_id: args?.providerFileId ?? null,
                provider_version_id: null,
                provider_file_group_id: null,
              },
              installation_id: null,
              desired: 'enabled',
              pin: { kind: 'unpinned' },
              priority: 0,
              added_at: '2026-09-02T10:00:00Z',
            };
            (allMembers()[String(args?.profileId)] ??= []).push(created);
            return created;
          }
          case 'remove_profile_member': {
            for (const list of Object.values(allMembers())) {
              const index = list.findIndex((candidate) => candidate.id === args?.memberId);
              if (index >= 0) list.splice(index, 1);
            }
            return undefined;
          }
          case 'set_member_state': {
            const selected = findMember(args?.memberId);
            selected.desired = args?.desired;
            return selected;
          }
          case 'set_member_pin': {
            const selected = findMember(args?.memberId);
            selected.pin = args?.pinned
              ? { kind: 'pinned', pinned_at: '2026-09-02T10:00:00Z', reason: args?.reason ?? null }
              : { kind: 'unpinned' };
            return selected;
          }
          case 'reorder_profile_member': {
            const selected = findMember(args?.memberId);
            selected.priority = args?.priority;
            return selected;
          }
          case 'plan_profile_activation':
            return {
              ...(state.preview as Record<string, unknown>),
              to_profile_id: args?.profileId,
            };
          case 'activate_profile': {
            emit({ type: 'started', stage: 'downloading', total: 4096 });
            emit({
              type: 'advanced',
              stage: 'downloading',
              completed: 4096,
              total: 4096,
              detail: 'archive.zip',
            });
            emit({ type: 'started', stage: 'filesystem_verification', total: 2 });
            emit({ type: 'finished', stage: 'filesystem_verification', success: true });
            const result = {
              ...(state.activation as Record<string, unknown>),
              to_profile_id: args?.profileId,
            };
            if (result.state === 'applied') {
              rows().forEach((candidate) => {
                candidate.is_active = candidate.id === args?.profileId;
              });
            }
            return result;
          }
          case 'cancel_operation':
            return undefined;
          case 'interrupted_operations':
            return state.interrupted;
          case 'roll_back':
            state.interrupted = [];
            return undefined;
          default:
            throw { code: 'internal', message: `no stub for ${command}` };
        }
      },
      listen: async (_event: string, handler: (payload: unknown) => void) => {
        listeners.push(handler);
        return () => {
          const index = listeners.indexOf(handler);
          if (index >= 0) listeners.splice(index, 1);
        };
      },
    };
  }, config);
}

function card(page: Page, name: string) {
  return page.locator('article.profile-card').filter({ has: page.getByRole('heading', { name }) });
}

test('profiles have honest loading and empty states', async ({ page }) => {
  await stubProfiles(page, { profiles: [], profilesDelay: 300 });
  await page.goto('/profiles');
  await expect(page.getByText('Loading profiles…')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'No profiles found' })).toBeVisible();
});

test('profile loading errors are actionable and never rendered as empty', async ({ page }) => {
  await stubProfiles(page, {
    errors: { profiles: { code: 'internal', message: 'profile catalog unavailable' } },
  });
  await page.goto('/profiles');
  await expect(page.getByRole('alert')).toHaveText('profile catalog unavailable');
  await expect(page.getByRole('heading', { name: 'No profiles found' })).toHaveCount(0);
});

test('a profile can be duplicated as a starting point', async ({ page }) => {
  await stubProfiles(page, {
    members: { 'profile-default': [member()], 'profile-quiet': [] },
  });
  await page.goto('/profiles');
  await card(page, 'Default').getByRole('button', { name: 'Duplicate' }).click();
  await expect(page.getByRole('status')).toContainText('Duplicated as Default copy');
  await expect(card(page, 'Default copy')).toBeVisible();
  await expect(page.getByText('nexus:107')).toBeVisible();
});

test('profiles can be created, renamed, shown, and deleted', async ({ page }) => {
  await stubProfiles(page);
  page.on('dialog', (dialog) => dialog.accept());
  await page.goto('/profiles');

  await page.getByLabel('New profile name').fill('Testing');
  await page.getByLabel('New profile description').fill('Temporary desired set');
  await page.getByRole('button', { name: 'Create profile' }).click();
  await expect(page.getByRole('heading', { name: 'Testing members' })).toBeVisible();

  await card(page, 'Testing').getByRole('button', { name: 'Rename' }).click();
  await page.getByLabel('Rename Testing').fill('Testing renamed');
  await page.getByRole('button', { name: 'Save name' }).click();
  await expect(card(page, 'Testing renamed')).toBeVisible();

  await card(page, 'Default').getByRole('button', { name: 'Show' }).click();
  await expect(page.getByRole('heading', { name: 'Default members' })).toBeVisible();
  await card(page, 'Testing renamed').getByRole('button', { name: 'Delete' }).click();
  await expect(card(page, 'Testing renamed')).toHaveCount(0);
});

test('members can be added, disabled, pinned, reordered with a signed priority, and removed', async ({
  page,
}) => {
  await stubProfiles(page);
  await page.goto('/profiles');
  await card(page, 'Quiet').getByRole('button', { name: 'Show' }).click();

  await page.getByLabel('Mod ID').fill('555');
  await page.getByLabel(/Provider file ID/).fill('file-555');
  await page.getByRole('button', { name: 'Add member' }).click();
  await expect(page.getByText('nexus:555')).toBeVisible();
  await expect(page.getByText('Download required')).toBeVisible();

  await page.getByRole('button', { name: 'Disable 555' }).click();
  await expect(page.getByRole('button', { name: 'Enable 555' })).toBeVisible();
  await page.getByRole('button', { name: 'Enable 555' }).click();
  await expect(page.getByRole('button', { name: 'Disable 555' })).toBeVisible();
  await page.getByRole('button', { name: 'Pin 555' }).click();
  await expect(page.getByRole('button', { name: 'Unpin 555' })).toBeVisible();
  await page.getByRole('button', { name: 'Unpin 555' }).click();
  await expect(page.getByRole('button', { name: 'Pin 555' })).toBeVisible();

  await page.getByLabel('Priority for 555').fill('-7');
  await page
    .getByLabel('Priority for 555')
    .locator('..')
    .getByRole('button', { name: 'Save' })
    .click();
  await expect(page.getByRole('status')).toContainText('Lower values deploy first');
  await expect(page.getByLabel('Priority for 555')).toHaveValue('-7');

  await page
    .getByRole('row')
    .filter({ hasText: 'nexus:555' })
    .getByRole('button', { name: 'Remove' })
    .click();
  await expect(page.getByText('nexus:555')).toHaveCount(0);
  await expect(page.getByRole('heading', { name: 'No members' })).toBeVisible();
});

test('blocked activation shows downloads, changes, conflicts, incomplete dependencies and stale baseline', async ({
  page,
}) => {
  const incomplete = dependency({
    outcome: { kind: 'unknown', reason: 'offline' },
    health: [{ profile_member_id: 'member-missing', health: 'unknown', unsatisfied: [] }],
    evidence: {
      fresh: 0,
      cached: 1,
      stale: 1,
      unavailable: 1,
      unsupported: 1,
      unknown_dlc: 1,
    },
  });
  await stubProfiles(page, {
    members: {
      'profile-default': [member()],
      'profile-quiet': [
        member({ id: 'member-missing', profile_id: 'profile-quiet', installation_id: null }),
      ],
    },
    dependency: incomplete,
    preview: preview({
      downloads: [{ member_id: 'member-missing', name: 'Cyber Engine Tweaks', bytes: 41_234_567 }],
      plan: {
        steps: [
          {
            kind: 'write',
            target: { root_key: 'game', path: 'archive/pc/mod/a.archive' },
            provider: {
              provider: { kind: 'installation', installation_id: 'installation-a' },
              hash: 'blake3:a',
              size: 10,
            },
            expected_previous: null,
          },
          {
            kind: 'delete',
            target: { root_key: 'game', path: 'r6/scripts/old.reds' },
            expected_previous: 'blake3:old',
          },
          {
            kind: 'write',
            target: { root_key: 'game', path: 'bin/plugin.dll' },
            provider: {
              provider: { kind: 'installation', installation_id: 'installation-new' },
              hash: 'blake3:new',
              size: 20,
            },
            expected_previous: 'blake3:previous',
          },
          {
            kind: 'write',
            target: { root_key: 'game', path: 'r6/scripts/shared.reds' },
            provider: {
              provider: { kind: 'unmanaged_backup', backup_id: 'backup-1' },
              hash: 'blake3:original',
              size: 30,
            },
            expected_previous: 'blake3:modded',
          },
        ],
        conflicts: [
          {
            target: { root_key: 'game', path: 'archive/pc/mod/a.archive' },
            providers: ['installation-a', 'installation-b'],
          },
        ],
      },
      dependency: incomplete,
      baseline_freshness: {
        kind: 'stale',
        captured: identity('18234000'),
        observed: identity('18300000'),
      },
      bytes_to_write: 91_234_567,
      ready: false,
      blockers: [
        { kind: 'cross_mod_conflict', target: 'game:archive/pc/mod/a.archive' },
        { kind: 'dependency_unsatisfied', member_id: 'member-missing' },
      ],
    }),
  });
  await page.goto('/profiles');
  await card(page, 'Quiet').getByRole('button', { name: 'Show' }).click();
  await page.getByRole('button', { name: 'Preview activation' }).click();

  const panel = page.getByTestId('activation-preview');
  await expect(panel).toContainText('Cyber Engine Tweaks');
  await expect(panel).toContainText('87.0 MiB');
  await expect(panel).toContainText('Activate retained file');
  await expect(panel).toContainText('Deactivate deployed file');
  await expect(panel).toContainText('activations, upgrades, downgrades, and restorations');
  await expect(panel).toContainText('Change selected version');
  await expect(panel).toContainText('Restore covered file');
  await expect(panel).toContainText('2 providers need a winner');
  await expect(page.getByTestId('activation-freshness')).toHaveText('Stale');
  await expect(page.getByTestId('dependency-outcome')).toContainText(
    'Dependency compatibility unknown',
  );
  await expect(panel).toContainText('Stale dependency data');
  await expect(panel).toContainText('Dependency data unavailable');
  await expect(panel).toContainText('Dependencies unsupported');
  await expect(panel).toContainText('DLC ownership unknown');
  await expect(page.getByTestId('activation-blockers')).toContainText('cross mod conflict');
  await expect(page.getByRole('button', { name: 'Activate profile' })).toBeDisabled();
});

test('a successful activation reports progress and only then marks the target active', async ({
  page,
}) => {
  await stubProfiles(page);
  await page.goto('/profiles');
  await card(page, 'Quiet').getByRole('button', { name: 'Show' }).click();
  await page.getByRole('button', { name: 'Preview activation' }).click();
  await page.getByRole('button', { name: 'Activate profile' }).click();

  await expect(page.getByTestId('activation-result')).toContainText('Profile activated');
  await expect(page.getByTestId('activation-progress')).toContainText('filesystem verification');
  await expect(card(page, 'Quiet').getByText('Active', { exact: true })).toBeVisible();
  await expect(card(page, 'Default').getByText('Active', { exact: true })).toHaveCount(0);
});

test('an in-progress activation can request cancellation by operation id', async ({ page }) => {
  await stubProfiles(page, {
    activation: {
      from_profile_id: 'profile-default',
      to_profile_id: 'profile-quiet',
      operation_id: 'operation-1',
      state: 'applying',
      started_at: '2026-09-02T10:00:00Z',
      finished_at: null,
      error: null,
    },
  });
  await page.goto('/profiles');
  await card(page, 'Quiet').getByRole('button', { name: 'Show' }).click();
  await page.getByRole('button', { name: 'Preview activation' }).click();
  await page.getByRole('button', { name: 'Activate profile' }).click();
  await page.getByRole('button', { name: 'Cancel activation' }).click();
  await expect(page.getByRole('button', { name: 'Cancellation requested…' })).toBeDisabled();
  const calls = await page.evaluate(
    () =>
      (
        window as unknown as {
          __ONERA_CALLS__: Array<{ command: string; args?: Record<string, unknown> }>;
        }
      ).__ONERA_CALLS__,
  );
  expect(calls).toContainEqual({
    command: 'cancel_operation',
    args: { operationId: 'operation-1' },
  });
});

test('a failed activation that rolls back keeps the source profile active', async ({ page }) => {
  await stubProfiles(page, {
    activation: {
      from_profile_id: 'profile-default',
      to_profile_id: 'profile-quiet',
      operation_id: 'operation-1',
      state: 'rolled_back',
      started_at: '2026-09-02T10:00:00Z',
      finished_at: '2026-09-02T10:01:00Z',
      error: 'verification failed',
    },
  });
  await page.goto('/profiles');
  await card(page, 'Quiet').getByRole('button', { name: 'Show' }).click();
  await page.getByRole('button', { name: 'Preview activation' }).click();
  await page.getByRole('button', { name: 'Activate profile' }).click();

  await expect(page.getByTestId('activation-result')).toContainText('Activation rolled back');
  await expect(page.getByTestId('activation-result')).toContainText(
    'previous profile remains active',
  );
  await expect(card(page, 'Default').getByText('Active', { exact: true })).toBeVisible();
  await expect(card(page, 'Quiet').getByText('Active', { exact: true })).toHaveCount(0);
});

test('a failed rollback requires recovery and never marks the target active', async ({ page }) => {
  await stubProfiles(page, {
    activation: {
      from_profile_id: 'profile-default',
      to_profile_id: 'profile-quiet',
      operation_id: 'operation-1',
      state: 'failed',
      started_at: '2026-09-02T10:00:00Z',
      finished_at: '2026-09-02T10:01:00Z',
      error: 'could not restore archive/pc/mod/a.archive',
    },
  });
  await page.goto('/profiles');
  await card(page, 'Quiet').getByRole('button', { name: 'Show' }).click();
  await page.getByRole('button', { name: 'Preview activation' }).click();
  await page.getByRole('button', { name: 'Activate profile' }).click();

  await expect(page.getByTestId('activation-result')).toContainText('Recovery required');
  await expect(
    page.getByTestId('activation-result').getByRole('link', { name: 'Open recovery' }),
  ).toBeVisible();
  await expect(card(page, 'Quiet').getByText('Active', { exact: true })).toHaveCount(0);
});

test('deleting the active profile displays the stable conflict error', async ({ page }) => {
  await stubProfiles(page);
  page.on('dialog', (dialog) => dialog.accept());
  await page.goto('/profiles');
  await card(page, 'Default').getByRole('button', { name: 'Delete' }).click();
  await expect(page.getByRole('alert')).toHaveAttribute('data-error-code', 'conflict');
  await expect(page.getByRole('alert')).toContainText('Activate another profile first');
});

test('restart recovery offers rollback without claiming the target profile is active', async ({
  page,
}) => {
  await stubProfiles(page, {
    interrupted: [
      {
        operation_id: 'operation-1',
        kind: 'reconcile',
        state: 'committing',
        recovery: 'continue_or_roll_back',
        committed_files: 2,
        staged_files: 5,
        created_at: '2026-09-02T10:00:00Z',
      },
    ],
  });
  await page.goto('/recovery');
  await expect(page.getByText(/target profile is not active/i)).toBeVisible();
  await page.getByRole('button', { name: 'Roll back' }).click();
  await expect(page.getByRole('status')).toContainText('previously active profile remains active');
  await expect(page.getByText(/nothing was interrupted/i)).toBeVisible();
});
