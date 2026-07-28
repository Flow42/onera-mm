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
  await stubBridge(page, { is_authenticated: false });
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
  await stubBridge(page, { is_authenticated: true, discover_games: [], local_games: [] });
  await page.goto('/');
  await expect(page).toHaveURL(/games/);
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
