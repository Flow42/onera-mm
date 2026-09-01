import { expect, test } from '@playwright/test';

/**
 * Frontend end-to-end tests.
 *
 * These drive the real SvelteKit build against a stubbed Tauri bridge, so they
 * cover the views and the wiring without needing a compiled desktop binary, a
 * keyring or a game on disk. The Rust side's own end-to-end coverage lives in
 * `crates/onera-app/tests/end_to_end.rs`.
 */

/** Install a fake bridge before the app's own modules load. */
async function stubBridge(
  page: import('@playwright/test').Page,
  responses: Record<string, unknown>,
) {
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

test('first launch sends the user to onboarding', async ({ page }) => {
  await stubBridge(page, {
    startup_status: { authenticated: false, recovery_required: false, inbox_count: 0 },
  });
  await page.goto('/');
  await expect(page).toHaveURL(/onboarding/);
  await expect(page.getByRole('heading', { name: /connect to nexus/i })).toBeVisible();
});

test('the API key field is masked', async ({ page }) => {
  await stubBridge(page, { is_authenticated: false });
  await page.goto('/onboarding');
  const field = page.getByLabel(/personal api key/i);
  await expect(field).toHaveAttribute('type', 'password');
});

test('a validated key shows the account', async ({ page }) => {
  await stubBridge(page, {
    is_authenticated: false,
    set_api_key: { username: 'TestUser', premium: true, provider_user_id: '1', email: null },
  });
  await page.goto('/onboarding');
  await page.getByLabel(/personal api key/i).fill('a-plausible-api-key-0123456789');
  await page.getByRole('button', { name: /validate and save/i }).click();
  await expect(page.getByText('TestUser')).toBeVisible();
});

test('an already-authenticated launch goes to the games list', async ({ page }) => {
  await stubBridge(page, {
    startup_status: { authenticated: true, recovery_required: false, inbox_count: 0 },
    discover_games: [],
    local_games: [],
  });
  await page.goto('/');
  await expect(page).toHaveURL(/games/);
});

test('startup prioritises recovery over the browser inbox', async ({ page }) => {
  await stubBridge(page, {
    startup_status: { authenticated: true, recovery_required: true, inbox_count: 1 },
    interrupted_operations: [],
  });
  await page.goto('/');
  await expect(page).toHaveURL(/recovery/);
});

test('an expired preview is explained after restart', async ({ page }) => {
  await stubBridge(page, {
    startup_status: {
      authenticated: true,
      recovery_required: false,
      inbox_count: 0,
      expired_plans: 1,
    },
    local_games: [],
    inbox_requests: [],
  });
  await page.goto('/');
  await expect(page).toHaveURL(/add\?expired=1/);
  await expect(page.getByText(/previous installation preview expired/i)).toBeVisible();
});

test('a browser request opens the durable add-mod inbox', async ({ page }) => {
  await stubBridge(page, {
    startup_status: { authenticated: true, recovery_required: false, inbox_count: 1 },
    local_games: [],
    inbox_requests: [
      {
        id: 'request-1',
        kind: 'add_mod',
        provider: 'nexus',
        game_slug: 'cyberpunk2077',
        provider_mod_id: '107',
        provider_file_id: null,
        state: 'queued',
        error: null,
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
      },
    ],
    fetch_mod: {
      mod_id: 'internal-1',
      name: 'Cyber Engine Tweaks',
      author: 'yamashi',
      needs_file_selection: false,
      files: [{ id: '9001', name: 'cet.zip', category: 'main', size: 13, is_primary: true }],
    },
  });
  await page.goto('/');
  await expect(page).toHaveURL(/add/);
  await expect(page.getByText('Cyber Engine Tweaks')).toBeVisible();
  await expect(page.getByText(/from your browser/i)).toBeVisible();
});

test('installed mods are rendered from the native read model', async ({ page }) => {
  await stubBridge(page, {
    local_games: [
      { id: 'game-1', adapter_id: 'cyberpunk2077', install_root: '/games/cp2077', confirmed: true },
    ],
    installed_mods: [
      {
        installation_id: 'installation-1',
        mod_id: 'mod-1',
        name: 'Cyber Engine Tweaks',
        version: '1.2.3',
        installed_at: '2026-01-01T00:00:00Z',
        update_available: false,
        latest_version: null,
      },
    ],
  });
  await page.goto('/mods');
  await expect(page.getByText('Cyber Engine Tweaks')).toBeVisible();
  await expect(page.getByText('1.2.3')).toBeVisible();
});

test('persisted download jobs are visible after navigation', async ({ page }) => {
  await stubBridge(page, {
    downloads: [
      {
        id: 'job-1',
        provider: 'nexus',
        game_slug: 'cyberpunk2077',
        provider_mod_id: '107',
        provider_file_id: '9001',
        filename: 'cet.zip',
        expected_size: 1000,
        expected_hash: null,
        temp_path: '/tmp/job.part',
        bytes_downloaded: 500,
        state: 'paused',
        attempts: 1,
        error: null,
        archive_id: null,
      },
    ],
  });
  await page.goto('/downloads');
  await expect(page.getByText('cet.zip')).toBeVisible();
  await expect(page.getByText('paused')).toBeVisible();
  await expect(page.getByText(/500 B \/ 1000 B/)).toBeVisible();
});

test('recovery reports when nothing was interrupted', async ({ page }) => {
  await stubBridge(page, { interrupted_operations: [] });
  await page.goto('/recovery');
  await expect(page.getByText(/nothing was interrupted/i)).toBeVisible();
});

test('an interrupted operation offers a rollback', async ({ page }) => {
  await stubBridge(page, {
    interrupted_operations: [
      {
        operation_id: 'op-1',
        kind: 'install',
        state: 'prepared',
        recovery: 'ContinueOrRollBack',
        committed_files: 0,
        staged_files: 3,
        created_at: '2026-01-01T00:00:00Z',
      },
    ],
  });
  await page.goto('/recovery');
  await expect(page.getByRole('button', { name: /roll back/i })).toBeVisible();
});
