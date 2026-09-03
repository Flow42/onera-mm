import { expect, test, type Page } from '@playwright/test';

const GAME = 'game-1';

const game = {
  id: GAME,
  adapter_id: 'cyberpunk2077',
  install_root: '/games/Cyberpunk 2077',
  confirmed: true,
};

const profile = {
  id: 'profile-default',
  local_game_id: GAME,
  name: 'Default',
  description: null,
  is_active: true,
  created_at: '2026-08-01T12:00:00Z',
  updated_at: '2026-09-01T18:22:00Z',
};

const member = {
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
};

const evidence = (over: Record<string, unknown> = {}) => ({
  fresh: 2,
  cached: 0,
  stale: 0,
  unavailable: 0,
  unsupported: 0,
  unknown_dlc: 0,
  ...over,
});

const solved = {
  outcome: {
    kind: 'update_set',
    select: [
      {
        provider: 'nexus',
        provider_mod_id: '107',
        provider_file_id: '9100',
        provider_version_id: 'v-9100',
        provider_file_group_id: 'g-107',
        profile_member_id: 'member-1',
        change: 'upgrade',
        display_name: 'Cyber Engine Tweaks 1.36.0',
      },
      {
        provider: 'nexus',
        provider_mod_id: '222',
        provider_file_id: '7000',
        provider_version_id: 'v-7000',
        provider_file_group_id: 'g-222',
        profile_member_id: 'member-2',
        change: 'downgrade',
        display_name: 'Codeware 1.9.0',
        reason: 'the newest Codeware is not compatible with the solved CET version.',
      },
    ],
    install: [],
  },
  health: [
    { profile_member_id: 'member-1', health: 'satisfied', unsatisfied: [] },
    { profile_member_id: 'member-2', health: 'satisfied', unsatisfied: [] },
  ],
  evidence: evidence(),
};

const preview = (over: Record<string, unknown> = {}) => ({
  profile_id: 'profile-default',
  dependency: solved,
  plan: {
    steps: [
      { kind: 'write', target: { root_key: 'game', path: 'bin/cet.dll' }, expected_previous: null },
    ],
    conflicts: [],
  },
  downloads: [{ member_id: 'member-1', name: 'Cyber Engine Tweaks 1.36.0', bytes: null }],
  bytes_to_write: 91_234_567,
  ready: true,
  blockers: [],
  fingerprint: 'b3:update-plan',
  ...over,
});

interface MockConfig {
  games?: unknown[];
  profiles?: unknown[];
  preview?: unknown;
  report?: unknown;
  errors?: Record<string, { code: string; message: string }>;
  planDelay?: number;
  /** Refuse the first apply with `conflict`, as a moved plan would. */
  refuseFirstApply?: boolean;
}

async function stubUpdates(page: Page, supplied: MockConfig = {}) {
  const config = {
    games: [game],
    profiles: [profile],
    preview: preview(),
    report: {
      profile_id: 'profile-default',
      operation_id: 'operation-1',
      state: 'applied',
      selected: [],
      started_at: '2026-09-02T10:00:00Z',
      finished_at: '2026-09-02T10:01:00Z',
      error: null,
    },
    errors: {},
    planDelay: 0,
    refuseFirstApply: false,
    ...supplied,
  };

  await page.addInitScript((settings) => {
    const state = structuredClone(settings) as typeof settings;
    const calls: { command: string; args?: Record<string, unknown> }[] = [];
    let refusalsLeft = state.refuseFirstApply ? 1 : 0;

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
            return state.profiles;
          case 'profile_members':
            return [member];
          case 'plan_compatible_updates':
            if (state.planDelay > 0) {
              await new Promise((resolve) => setTimeout(resolve, state.planDelay));
            }
            return state.preview;
          case 'apply_compatible_updates':
            if (refusalsLeft > 0) {
              refusalsLeft -= 1;
              throw { code: 'conflict', message: 'the compatible set is out of date' };
            }
            return state.report;
          default:
            throw { code: 'internal', message: `no stub for ${command}` };
        }
      },
      listen: async () => () => {},
    };
  }, config);
}

test('the whole profile is solved once, not one newest version per mod', async ({ page }) => {
  await stubUpdates(page);
  await page.goto('/updates');

  await expect(page.getByTestId('updates-headline')).toContainText(
    'One compatible set for the whole profile',
  );
  await expect(page.getByTestId('updates-headline')).toContainText('not chosen per mod');
  const plan = page.getByTestId('updates-plan');
  await expect(plan).toContainText('Cyber Engine Tweaks 1.36.0');
  await expect(plan).toContainText('Codeware 1.9.0');
  await expect(plan).toContainText('Downgrade');
  await expect(plan).toContainText('not compatible with the solved CET version');
  // One acceptance for the set; no per-row accept exists.
  await expect(page.getByTestId('updates-apply')).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Update this mod' })).toHaveCount(0);
  await expect(page.getByTestId('updates-headline')).toContainText('size unknown');

  await page.getByTestId('updates-apply').click();
  await expect(page.getByTestId('updates-result')).toContainText('Profile updated');
});

test('loading, empty and failure are three different states', async ({ page }) => {
  await stubUpdates(page, { planDelay: 400 });
  await page.goto('/updates');
  await expect(page.getByTestId('updates-loading')).toBeVisible();
  await expect(page.getByTestId('updates-headline')).toBeVisible();

  await stubUpdates(page, {
    preview: preview({
      dependency: { outcome: { kind: 'compatible' }, health: [], evidence: evidence() },
      plan: { steps: [], conflicts: [] },
      downloads: [],
      bytes_to_write: 0,
    }),
  });
  await page.goto('/updates');
  await expect(page.getByTestId('updates-headline')).toContainText('No compatible update');
  await expect(page.getByTestId('updates-apply')).toBeDisabled();

  await stubUpdates(page, {
    errors: {
      plan_compatible_updates: { code: 'provider_error', message: 'the provider is unreachable' },
    },
  });
  await page.goto('/updates');
  await expect(page.getByTestId('updates-error')).toContainText('the provider is unreachable');
  await expect(page.getByTestId('updates-error')).toContainText('The profile was not changed');
  await expect(page.getByText('No compatible update')).toHaveCount(0);
});

test('offline operation shows labelled cached data and never calls it current', async ({
  page,
}) => {
  await stubUpdates(page, {
    preview: preview({
      dependency: {
        outcome: { kind: 'compatible' },
        health: [{ profile_member_id: 'member-1', health: 'satisfied', unsatisfied: [] }],
        evidence: evidence({ fresh: 0, cached: 3, stale: 2 }),
      },
      plan: { steps: [], conflicts: [] },
      downloads: [],
      bytes_to_write: 0,
    }),
  });
  await page.goto('/updates');

  await expect(page.getByTestId('updates-headline')).toContainText('incomplete data');
  await expect(page.getByTestId('updates-offline')).toContainText('never presented as current');
  await expect(page.getByTestId('updates-plan-evidence')).toContainText('Cached dependency data');
  await expect(page.getByTestId('updates-plan-evidence')).toContainText('Stale dependency data');
});

test('an unsolved profile offers no update action at all', async ({ page }) => {
  await stubUpdates(page, {
    preview: preview({
      dependency: {
        outcome: { kind: 'unknown', reason: 'dependency data is unavailable' },
        health: [{ profile_member_id: 'member-1', health: 'unknown', unsatisfied: [] }],
        evidence: evidence({ fresh: 0, unavailable: 2 }),
      },
      ready: false,
      blockers: [{ kind: 'dependency_unsatisfied', member_id: 'member-1' }],
    }),
  });
  await page.goto('/updates');

  await expect(page.getByTestId('updates-headline')).toContainText(
    'Dependency compatibility unknown',
  );
  await expect(page.getByTestId('updates-blockers')).toContainText('dependency unsatisfied');
  await expect(page.getByTestId('updates-apply')).toBeDisabled();
  await expect(
    page.getByTestId('updates-plan').getByRole('button', { name: /^Update and downgrade/ }),
  ).toHaveCount(0);
});

test('a compatible set that moved on is refused and re-solved rather than applied', async ({
  page,
}) => {
  await stubUpdates(page, { refuseFirstApply: true });
  await page.goto('/updates');
  await page.getByTestId('updates-apply').click();

  await expect(page.getByTestId('updates-stale-plan')).toContainText('This plan is out of date');
  await expect(page.getByTestId('updates-result')).toHaveCount(0);

  await page.getByTestId('updates-apply').click();
  await expect(page.getByTestId('updates-result')).toContainText('Profile updated');

  const calls = await page.evaluate(
    () =>
      (
        window as unknown as {
          __ONERA_CALLS__: Array<{ command: string; args?: Record<string, unknown> }>;
        }
      ).__ONERA_CALLS__,
  );
  expect(calls).toContainEqual({
    command: 'apply_compatible_updates',
    args: { profileId: 'profile-default', expectedFingerprint: 'b3:update-plan' },
  });
});

test('a rolled-back update is never reported as applied', async ({ page }) => {
  await stubUpdates(page, {
    report: {
      profile_id: 'profile-default',
      operation_id: 'operation-1',
      state: 'rolled_back',
      selected: [],
      started_at: '2026-09-02T10:00:00Z',
      finished_at: '2026-09-02T10:01:00Z',
      error: 'verification failed',
    },
  });
  await page.goto('/updates');
  await page.getByTestId('updates-apply').click();

  await expect(page.getByTestId('updates-result')).toContainText('Update rolled back');
  await expect(page.getByTestId('updates-result')).toContainText('verification failed');
  await expect(page.getByText('Profile updated')).toHaveCount(0);
});

test('a game with no profile says so instead of claiming everything is up to date', async ({
  page,
}) => {
  await stubUpdates(page, { profiles: [] });
  await page.goto('/updates');

  await expect(page.getByRole('heading', { name: 'No profiles' })).toBeVisible();
  await expect(page.getByText('Everything is up to date')).toHaveCount(0);
});
