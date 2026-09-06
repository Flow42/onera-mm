import { expect, test } from '@playwright/test';

/**
 * The library views: the rail's shape, a game's two directories, and what a mod
 * put on disk.
 *
 * These drive the real build against a stubbed bridge, so they cover what the
 * user is actually shown — a submenu that only exists once a game is chosen, a
 * staging directory that can be moved, a mod that opens to reveal its files —
 * without a compiled binary or a game on disk.
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

const GAMES = [
  { id: 'game-1', adapter_id: 'cyberpunk2077', install_root: '/games/cp2077', confirmed: true },
  {
    id: 'game-2',
    adapter_id: 'skyrimspecialedition',
    install_root: '/games/skyrim',
    confirmed: true,
  },
];

const PATHS = {
  install_root: '/games/cp2077',
  staging_root: '/home/user/.local/state/onera/staging',
  staging_is_default: true,
  staging_entries: 0,
};

test('the rail lists games and shows a submenu only for the one in context', async ({ page }) => {
  await stubBridge(page, { local_games: GAMES, game_icon: null, installed_mods: [] });
  const rail = page.getByRole('navigation', { name: 'Sections' });

  await page.goto('/games');
  await expect(rail.getByRole('link', { name: 'cp2077' })).toBeVisible();
  await expect(rail.getByRole('link', { name: 'skyrim' })).toBeVisible();
  // Nothing is selected, so no game's sections are on offer.
  await expect(rail.getByRole('link', { name: 'Profiles' })).toHaveCount(0);
  await expect(rail.getByRole('link', { name: 'Integrity' })).toHaveCount(0);

  await rail.getByRole('link', { name: 'cp2077' }).click();
  await expect(page).toHaveURL(/games\/game-1/);

  // The submenu belongs to the game that was chosen, and only to it.
  const sections = rail.locator('.submenu');
  await expect(sections).toHaveCount(1);
  await expect(sections.getByRole('link', { name: 'Profiles' })).toBeVisible();
  await expect(sections.getByRole('link', { name: 'Integrity' })).toBeVisible();
  await expect(
    rail.locator('li', { has: page.getByRole('link', { name: 'skyrim' }) }).locator('.submenu'),
  ).toHaveCount(0);
});

test('a game shows both its directories and can give downloads a home of its own', async ({
  page,
}) => {
  await stubBridge(page, {
    local_games: GAMES,
    game_icon: null,
    installed_mods: [],
    game_paths: PATHS,
    download_paths: {
      root: '/home/user/.local/share/onera/archives',
      scope: 'default',
      inherited: '/home/user/.local/share/onera/archives',
      archives: 3,
      bytes: 1024,
    },
    pick_download_root: {
      root: '/mnt/games/cp2077-mods',
      moved: 3,
      bytes: 4096,
      previous: '/home/user/.local/share/onera/archives',
    },
  });

  await page.goto('/games/game-1');
  await expect(page.getByText('/games/cp2077')).toBeVisible();
  await expect(page.getByText('/home/user/.local/share/onera/archives')).toBeVisible();
  // Where the setting came from is part of the answer: this game has not
  // chosen one, so it is using the directory everything else uses.
  await expect(page.getByText("Onera's own")).toBeVisible();
  await expect(page.getByText('3 archives')).toBeVisible();

  await page.getByRole('button', { name: 'Change for this game…' }).click();
  await expect(page.getByText(/now go to \/mnt\/games\/cp2077-mods/)).toBeVisible();
  await expect(page.getByText(/3 archives were moved there/)).toBeVisible();
});

test('the shared download directory is changed in settings', async ({ page }) => {
  await stubBridge(page, {
    account: { provider_user_id: '1', username: 'TestUser', premium: false, email: null },
    diagnostics: { version: '0.1.0' },
    download_paths: {
      root: '/home/user/.local/share/onera/archives',
      scope: 'default',
      inherited: null,
      archives: 12,
      bytes: 1024,
    },
    pick_download_root: {
      root: '/mnt/big-disk/onera',
      moved: 12,
      bytes: 999,
      previous: '/home/user/.local/share/onera/archives',
    },
    local_games: [],
  });

  await page.goto('/settings');
  await expect(page.getByRole('heading', { name: 'Downloads' })).toBeVisible();
  await expect(page.getByText('/home/user/.local/share/onera/archives')).toBeVisible();
  // A plain string matcher, because it has to survive the paragraph wrapping.
  await expect(page.getByText("set on that game's page")).toBeVisible();

  await page.getByRole('button', { name: 'Change…' }).click();
  await expect(page.getByText(/Downloads now go to \/mnt\/big-disk\/onera/)).toBeVisible();
  await expect(page.getByText(/12 archives were moved there/)).toBeVisible();
});

test('a refused download directory says why and changes nothing', async ({ page }) => {
  await page.addInitScript(() => {
    // @ts-expect-error - injected for the app to pick up.
    window.__ONERA_TEST_BRIDGE__ = {
      invoke: async (command: string) => {
        if (command === 'local_games') {
          return [
            {
              id: 'game-1',
              adapter_id: 'cyberpunk2077',
              install_root: '/games/cp2077',
              confirmed: true,
            },
          ];
        }
        if (command === 'installed_mods') return [];
        if (command === 'game_icon') return null;
        if (command === 'game_paths') {
          return {
            install_root: '/games/cp2077',
            staging_root: '/home/user/.local/state/onera/staging',
            staging_is_default: true,
            staging_entries: 0,
          };
        }
        if (command === 'download_paths') {
          return {
            root: '/home/user/.local/share/onera/archives',
            scope: 'default',
            inherited: '/home/user/.local/share/onera/archives',
            archives: 0,
            bytes: 0,
          };
        }
        if (command === 'pick_download_root') {
          throw {
            code: 'internal',
            message:
              '/games/cp2077/mods is inside /games/cp2077 — an archive kept inside a game would be read as a modified game file',
          };
        }
        throw { code: 'internal', message: `no stub for ${command}` };
      },
      listen: async () => () => {},
    };
  });

  await page.goto('/games/game-1');
  await page.getByRole('button', { name: 'Change for this game…' }).click();
  await expect(page.getByRole('alert')).toContainText('is inside /games/cp2077');
  await expect(page.getByText('/home/user/.local/share/onera/archives')).toBeVisible();
});

test('a mod opens to show the files it owns and a way into them', async ({ page }) => {
  await stubBridge(page, {
    local_games: [GAMES[0]],
    game_icon: null,
    mod_artwork: null,
    installed_mods: [
      {
        installation_id: 'installation-1',
        mod_id: 'mod-1',
        name: 'ArchiveXL',
        author: 'psiberx',
        version: '1.27.1',
        installed_at: '2026-01-01T00:00:00Z',
        published_at: '2025-12-01T00:00:00Z',
        update_available: false,
        latest_version: null,
        latest_published_at: null,
        game_slug: 'cyberpunk2077',
        provider_mod_id: '4198',
        thumbnail_url: null,
      },
    ],
    mod_contents: {
      kind: 'installed',
      browse: '/games/cp2077',
      total: 14,
      entries: [
        {
          root_key: 'game',
          path: 'archive/pc/mod/ArchiveXL.archive',
          absolute: '/games/cp2077/archive/pc/mod/ArchiveXL.archive',
          exists: true,
          size: 2048,
        },
        {
          root_key: 'game',
          path: 'red4ext/plugins/ArchiveXL/ArchiveXL.dll',
          absolute: '/games/cp2077/red4ext/plugins/ArchiveXL/ArchiveXL.dll',
          exists: false,
          size: null,
        },
      ],
    },
    browse_mod_files: '/games/cp2077',
  });

  await page.goto('/mods');
  const card = page.getByRole('listitem').filter({ hasText: 'ArchiveXL' });
  // Closed until it is asked for: the files are read on the first expand.
  await expect(card.getByText('ArchiveXL.archive')).toHaveCount(0);

  await card.getByRole('button', { name: 'ArchiveXL' }).click();
  await expect(card.getByText('ArchiveXL.archive')).toBeVisible();
  // Directories are folded, so the two files share their branches.
  await expect(card.getByText('archive', { exact: true })).toBeVisible();
  await expect(card.getByText('2.0 KiB')).toBeVisible();
  // A file that is no longer there is shown as missing rather than hidden.
  await expect(card.getByText('missing')).toBeVisible();
  // The cap is honest about what it left out.
  await expect(card.getByText(/14 files — 12 more files not shown/)).toBeVisible();

  await expect(card.getByRole('button', { name: 'Browse in files' })).toBeEnabled();
  await card.getByRole('button', { name: 'ArchiveXL' }).click();
  await expect(card.getByText('ArchiveXL.archive')).toHaveCount(0);
});
