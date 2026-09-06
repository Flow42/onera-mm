/**
 * Game icons, fetched once and shared by every view that draws one.
 *
 * An icon is read from the game's installation directory, which is a disk walk
 * — cheap, but not free, and the rail, the games list and a game's own page all
 * want the same picture. One cache for the window's lifetime means that walk
 * happens once per game rather than once per view that mounts.
 *
 * A game with no icon is remembered as having none, so a fruitless search is
 * not repeated either. Failure is silent by design: artwork is decoration, and
 * a game whose directory cannot be read is still a game.
 */

import { commands } from './bridge';

/** Icons by game id: a data URI, or null for a game that has none. */
const icons = $state<Record<string, string | null>>({});

/**
 * Games whose search is in flight, so it is not started twice.
 *
 * Deliberately outside the reactive state: nothing renders from it, and a
 * reactive read here would re-run every effect that draws an icon each time a
 * request starts or finishes.
 */
const pending: Record<string, true> = {};

/**
 * The icon for one game, starting the search the first time it is asked for.
 *
 * @param gameId - The local game id.
 * @returns The data URI, or null while it is unknown or if there is none.
 */
export function gameIcon(gameId: string): string | null {
  if (!(gameId in icons) && pending[gameId] !== true) {
    pending[gameId] = true;
    void commands
      .gameIcon(gameId)
      .then((uri) => {
        icons[gameId] = uri;
      })
      .catch(() => {
        icons[gameId] = null;
      })
      .finally(() => {
        delete pending[gameId];
      });
  }
  return icons[gameId] ?? null;
}

/**
 * Forget a game's icon, so the next request searches again.
 *
 * @param gameId - The local game id, or nothing to forget every game.
 */
export function forgetGameIcon(gameId?: string): void {
  if (gameId === undefined) {
    for (const key of Object.keys(icons)) delete icons[key];
    return;
  }
  delete icons[gameId];
}
