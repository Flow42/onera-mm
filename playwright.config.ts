import { defineConfig, devices } from '@playwright/test';

/**
 * End-to-end tests for the desktop frontend.
 *
 * These drive the SvelteKit application in a browser against a mocked Tauri
 * bridge, so they exercise the real views and view-models without needing a
 * built desktop binary or a real game on disk. The Rust side has its own
 * end-to-end coverage in `crates/onera-app/tests/end_to_end.rs`.
 */
export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: 'http://localhost:4173',
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'pnpm --filter onera-desktop preview --port 4173',
    url: 'http://localhost:4173',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
