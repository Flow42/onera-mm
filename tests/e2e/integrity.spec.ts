import { expect, test, type Page } from '@playwright/test';

/**
 * The Game Integrity panel, driven against a stubbed Tauri bridge.
 *
 * These cover the states the panel exists to keep honest: unknown freshness is
 * not freshness, a local snapshot says so, a quick check is never clean, a
 * capture cannot start over active mods or without the store-verification
 * confirmation, and returning to clean reports what it refuses to touch.
 */

const GAME = '9a1c0000-0000-0000-0000-000000000001';

const identity = (build: string | null) => ({
  store: 'steam',
  app_id: '1091500',
  build_id: build,
  branch: null,
  depots: [{ depot_id: '1091501', manifest_id: '77' }],
  manifest_path: '/games/steamapps/appmanifest_1091500.acf',
  observed_at: '2026-09-01T10:00:00Z',
});

const baseline = (over: Record<string, unknown> = {}) => ({
  id: '3f2b0000-0000-0000-0000-000000000002',
  local_game_id: GAME,
  source: 'store_verified_capture',
  build_identity: identity('18320471'),
  adapter_id: 'cyberpunk2077',
  reported_version: '2.21',
  status: 'current',
  captured_at: '2026-09-01T10:04:12Z',
  scope_fingerprint: 'b3',
  file_count: 41233,
  total_bytes: 71234567890,
  ...over,
});

const counts = (over: Record<string, number> = {}) => ({
  matching: 41233,
  modified: 0,
  missing: 0,
  extra_managed: 0,
  extra_unknown: 0,
  unreadable: 0,
  special: 0,
  ...over,
});

const localGames = [
  { id: GAME, adapter_id: 'cyberpunk2077', install_root: '/games/Cyberpunk 2077', confirmed: true },
];

/** Install a fake bridge before the app's own modules load. */
async function stubBridge(page: Page, responses: Record<string, unknown>) {
  await page.addInitScript((table) => {
    // @ts-expect-error - injected for the app to pick up.
    window.__ONERA_TEST_BRIDGE__ = {
      invoke: async (command: string) => {
        if (!(command in table)) {
          throw { code: 'internal', message: `no stub for ${command}` };
        }
        return (table as Record<string, unknown>)[command];
      },
      listen: async () => () => {},
    };
  }, responses);
}

test('an uncaptured installation is offered a capture, not called fresh', async ({ page }) => {
  await stubBridge(page, {
    local_games: localGames,
    baseline_status: {
      baseline: null,
      freshness: { kind: 'none' },
      observed_build_identity: identity('18320471'),
      active_mod_count: 0,
      capture_blocked_reason: null,
    },
  });
  await page.goto('/integrity');
  await expect(page.getByTestId('freshness')).toHaveText('Not captured');
  await expect(page.getByRole('heading', { name: /capture a baseline/i })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Capture' })).toBeVisible();
});

test('an unverifiable baseline is shown as such, never as fresh', async ({ page }) => {
  await stubBridge(page, {
    local_games: localGames,
    baseline_status: {
      baseline: baseline({ source: 'local_snapshot', build_identity: null }),
      freshness: {
        kind: 'unknown',
        reason: 'this installation was added manually, so Steam has no record of its build',
      },
      observed_build_identity: null,
      active_mod_count: 0,
      capture_blocked_reason: null,
    },
  });
  await page.goto('/integrity');
  await expect(page.getByTestId('freshness')).toHaveText('Cannot be verified');
  await expect(page.getByText(/not store-verified/i)).toBeVisible();
  await expect(page.getByText(/not that they were ever correct/i)).toBeVisible();
});

test('a changed build is stale and offers to replace the baseline', async ({ page }) => {
  await stubBridge(page, {
    local_games: localGames,
    baseline_status: {
      baseline: baseline(),
      freshness: {
        kind: 'stale',
        captured: identity('18320471'),
        observed: identity('18400000'),
      },
      observed_build_identity: identity('18400000'),
      active_mod_count: 0,
      capture_blocked_reason: null,
    },
  });
  await page.goto('/integrity');
  await expect(page.getByTestId('freshness')).toHaveText('Stale');
  await expect(page.getByText(/18400000/).first()).toBeVisible();
  await expect(page.getByRole('button', { name: /replace stale baseline/i })).toBeVisible();
});

test('capture is refused while Onera mods are active', async ({ page }) => {
  await stubBridge(page, {
    local_games: localGames,
    baseline_status: {
      baseline: null,
      freshness: { kind: 'none' },
      observed_build_identity: identity('18320471'),
      active_mod_count: 3,
      capture_blocked_reason:
        '3 Onera mod(s) are active; reconcile to an empty desired state before capturing a baseline',
    },
  });
  await page.goto('/integrity');
  await expect(page.getByText(/3 Onera mod\(s\) are active/)).toBeVisible();
  await expect(page.getByRole('button', { name: 'Capture' })).toHaveCount(0);
});

test('a store-verified capture waits for the explicit confirmation', async ({ page }) => {
  await stubBridge(page, {
    local_games: localGames,
    baseline_status: {
      baseline: null,
      freshness: { kind: 'none' },
      observed_build_identity: identity('18320471'),
      active_mod_count: 0,
      capture_blocked_reason: null,
    },
    plan_baseline_capture: {
      roots: [{ key: 'game', kind: 'game_install', path: '/games/Cyberpunk 2077' }],
      exclusions: [
        {
          root_key: 'game',
          pattern: { kind: 'prefix', path: 'r6/cache' },
          reason: 'cache',
          note: 'Redscript recompiles this on launch',
        },
      ],
      estimated_files: 41233,
      estimated_bytes: 71234567890,
      source: 'store_verified_capture',
      requires_store_verification: true,
      capture_blocked_reason: null,
    },
  });
  await page.goto('/integrity');
  const capture = page.getByRole('button', { name: 'Capture' });
  await expect(capture).toBeDisabled();

  await page.getByRole('button', { name: /what will be scanned/i }).click();
  await expect(page.getByText(/Redscript recompiles this on launch/)).toBeVisible();
  await expect(capture).toBeDisabled();

  await page.getByLabel(/verify installed files/i).check();
  await expect(capture).toBeEnabled();
});

test('a quick check is never presented as clean', async ({ page }) => {
  await stubBridge(page, {
    local_games: localGames,
    baseline_status: {
      baseline: baseline(),
      freshness: { kind: 'fresh' },
      observed_build_identity: identity('18320471'),
      active_mod_count: 0,
      capture_blocked_reason: null,
    },
    verify_baseline: {
      baseline_id: baseline().id,
      scan_run_id: '77a',
      state: 'completed',
      evidence: 'metadata_only',
      scope_fingerprint: 'b3',
      findings: [],
      counts: counts(),
      verified_at: '2026-09-02T09:12:00Z',
    },
  });
  await page.goto('/integrity');
  await page.getByRole('button', { name: /quick check/i }).click();
  await expect(page.getByTestId('verdict')).not.toHaveText('Clean');
  await expect(page.getByText(/never that nothing did/i).first()).toBeVisible();
});

test('a full check reports each difference with what it means', async ({ page }) => {
  await stubBridge(page, {
    local_games: localGames,
    baseline_status: {
      baseline: baseline(),
      freshness: { kind: 'fresh' },
      observed_build_identity: identity('18320471'),
      active_mod_count: 0,
      capture_blocked_reason: null,
    },
    verify_baseline: {
      baseline_id: baseline().id,
      scan_run_id: '77a',
      state: 'completed',
      evidence: 'content_hashed',
      scope_fingerprint: 'b3',
      findings: [
        {
          root_key: 'game',
          path: 'r6/scripts/thing.reds',
          classification: 'extra_unknown',
          expected: null,
          observed: 'blake3:aa',
          detail: null,
        },
      ],
      counts: counts({ extra_unknown: 1 }),
      verified_at: '2026-09-02T09:12:00Z',
    },
  });
  await page.goto('/integrity');
  await page.getByRole('button', { name: /full check/i }).click();
  await expect(page.getByTestId('verdict')).toHaveText('Differences found');
  await expect(page.getByText('game:r6/scripts/thing.reds')).toBeVisible();
  await expect(page.getByText('Unknown extra')).toBeVisible();
  await expect(page.getByText(/never deletes it for you/i)).toBeVisible();
});

test('return to clean reports what it will not touch, before and after', async ({ page }) => {
  const context = {
    restorable: [{ root_key: 'game', path: 'archive/pc/mod/a.archive', from: 'backup' }],
    needs_store_repair: [
      { root_key: 'game', path: 'bin/x64/Cyberpunk2077.exe', classification: 'modified' },
    ],
    unknown_extras: [{ root_key: 'game', path: 'r6/scripts/mystery.reds' }],
  };
  await stubBridge(page, {
    local_games: localGames,
    baseline_status: {
      baseline: baseline(),
      freshness: { kind: 'fresh' },
      observed_build_identity: identity('18320471'),
      active_mod_count: 1,
      capture_blocked_reason: '1 Onera mod(s) are active',
    },
    plan_return_to_clean: { plan: { steps: [{}, {}] }, ...context },
    apply_return_to_clean: {
      plan: { steps: [{}, {}] },
      restored: context.restorable,
      needs_store_repair: context.needs_store_repair,
      unknown_extras: context.unknown_extras,
      verification: {
        baseline_id: baseline().id,
        scan_run_id: '77b',
        state: 'completed',
        evidence: 'content_hashed',
        scope_fingerprint: 'b3',
        findings: [],
        counts: counts({ modified: 1, extra_unknown: 1 }),
        verified_at: '2026-09-02T09:20:00Z',
      },
      clean: false,
    },
  });
  await page.goto('/integrity');

  await page.getByRole('button', { name: 'Preview' }).click();
  await expect(page.getByTestId('clean-preview')).toContainText('1 file(s) can be restored');
  await expect(page.getByText(/need the store's own repair/i)).toBeVisible();
  await expect(page.getByText(/Onera never deletes these/i)).toBeVisible();

  await page.getByRole('button', { name: 'Return to clean' }).click();
  await expect(page.getByTestId('clean-report')).toContainText('1 file(s) restored');
  await expect(page.getByText(/differences above remain/i)).toBeVisible();
});

test('first game setup recommends capturing a baseline before installing', async ({ page }) => {
  await stubBridge(page, {
    discover_games: [
      {
        adapter_id: 'cyberpunk2077',
        provider_slug: 'cyberpunk2077',
        name: 'Cyberpunk 2077',
        install_root: '/games/Cyberpunk 2077',
        compat_prefix: null,
        user_data_roots: [],
        source: 'steam_native',
        validation: { valid: true, reported_version: '2.21', findings: [] },
      },
    ],
    local_games: [],
    confirm_game: GAME,
  });
  await page.goto('/games');
  await page.getByRole('button', { name: 'Confirm' }).click();
  const recommendation = page.getByTestId('baseline-recommendation');
  await expect(recommendation).toBeVisible();
  await expect(recommendation).toContainText('before your first install');
  await expect(recommendation.getByRole('link', { name: /capture a baseline/i })).toHaveAttribute(
    'href',
    `/integrity?game=${GAME}`,
  );
});
