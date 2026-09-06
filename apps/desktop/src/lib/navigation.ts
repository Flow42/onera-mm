/**
 * The shape of the menu, and the direction a move through it travels.
 *
 * The application is a hierarchy: the games list is the top of it, one game is
 * a level down, and a game's sections are a level below that. The shell needs
 * three pure answers about any address — how deep it is, which game it belongs
 * to, and what a game's sections are — so they live here rather than in the
 * layout, where they could not be tested without a browser.
 */

export { initialsOf } from './mod-card';

/** One entry in a game's submenu. */
export interface GameSection {
  /** Stable key, used for list identity. */
  key: string;
  label: string;
  /** One line saying what the section is for, shown on its card. */
  description: string;
  href: string;
  /** The path the entry highlights for, without the query. */
  match: string;
  /** A short mark for the card and the rail; decorative only. */
  glyph: string;
}

/**
 * How far down the hierarchy an address sits.
 *
 * A plain object rather than an enum: the values are compared as numbers and
 * this keeps the module transpilable file by file, the way the rest of the
 * frontend is built.
 */
export const Level = {
  /** Outside the hierarchy: startup, onboarding. */
  Outside: 0,
  /** The games list. */
  Games: 1,
  /** One game's submenu. */
  Game: 2,
  /** A section within a game, or a global leaf such as settings. */
  Section: 3,
} as const;

/** One of the {@link Level} values. */
export type Level = (typeof Level)[keyof typeof Level];

/**
 * The depth of one address.
 *
 * @param pathname - The path being shown, without query or hash.
 * @returns The level it belongs to.
 */
export function depthOf(pathname: string): Level {
  const path = normalise(pathname);
  if (path === '' || path === '/' || path === '/onboarding') {
    return Level.Outside;
  }
  if (path === '/games') {
    return Level.Games;
  }
  if (path.startsWith('/games/')) {
    return Level.Game;
  }
  return Level.Section;
}

/** Which way a move between two addresses travels. */
export type Direction = 'forward' | 'back' | 'none';

/**
 * The direction of a move, so the shell can animate it.
 *
 * Going deeper travels forward: the new view arrives from the right, as though
 * the menu had slid one step left. Coming back travels the other way. A move
 * between two addresses at the same level is neither, and is not animated as
 * a change of level.
 *
 * @param from - The address being left.
 * @param to - The address being opened.
 * @returns The direction of travel.
 */
export function directionBetween(from: string, to: string): Direction {
  const before = depthOf(from);
  const after = depthOf(to);
  if (after === before) {
    return 'none';
  }
  return after > before ? 'forward' : 'back';
}

/**
 * The game an address is about, if any.
 *
 * A game reaches its sections as a query parameter, because the sections are
 * routes in their own right and are also reachable without a game; the game's
 * own submenu carries it in the path.
 *
 * @param pathname - The path being shown.
 * @param search - The query string, `?game=…` included.
 * @returns The local game id, or null when the address is not about one.
 */
export function gameOf(pathname: string, search: string): string | null {
  const path = normalise(pathname);
  if (path.startsWith('/games/')) {
    const id = path.slice('/games/'.length).split('/')[0] ?? '';
    return id.length > 0 ? decodeURIComponent(id) : null;
  }
  const requested = new URLSearchParams(search).get('game');
  return requested !== null && requested.length > 0 ? requested : null;
}

/**
 * The submenu for one game.
 *
 * Every section that is scoped to a game carries the game with it, so opening
 * one from here never lands on a different installation than the card that was
 * clicked. Downloads is deliberately absent: the queue is shared across games,
 * so it is a global section rather than one of these.
 *
 * @param gameId - The local game id.
 * @returns The sections, in the order they are shown.
 */
export function gameSections(gameId: string): GameSection[] {
  const game = encodeURIComponent(gameId);
  return [
    {
      key: 'mods',
      label: 'Mods',
      description: 'Everything installed for this game, with its artwork and versions.',
      href: `/mods?game=${game}`,
      match: '/mods',
      glyph: '◈',
    },
    {
      key: 'profiles',
      label: 'Profiles',
      description:
        'Desired state: which mods are enabled, in which order, and switching between sets.',
      href: `/profiles?game=${game}`,
      match: '/profiles',
      glyph: '▤',
    },
    {
      key: 'updates',
      label: 'Updates',
      description: 'One compatible set solved for the whole enabled profile.',
      href: `/updates?game=${game}`,
      match: '/updates',
      glyph: '↑',
    },
    {
      key: 'add',
      label: 'From the browser',
      description: 'Mods the extension sent over, including any plan waiting for a decision.',
      href: `/add?game=${game}`,
      match: '/add',
      glyph: '↓',
    },
    {
      key: 'integrity',
      label: 'Integrity',
      description: 'The baseline, verification against it, and the way back to a clean install.',
      href: `/integrity?game=${game}`,
      match: '/integrity',
      glyph: '◇',
    },
    {
      key: 'ownership',
      label: 'Ownership',
      description: 'Which mod provides one file, and what it covered up.',
      href: `/ownership?game=${game}`,
      match: '/ownership',
      glyph: '⊞',
    },
  ];
}

/** The sections that are not about any one game. */
export function globalSections(): GameSection[] {
  return [
    {
      key: 'downloads',
      label: 'Downloads',
      description: 'The transfer queue, which is shared across every game.',
      href: '/downloads',
      match: '/downloads',
      glyph: '⇣',
    },
    {
      key: 'recovery',
      label: 'Recovery',
      description: 'Operations that were interrupted, and rolling one back.',
      href: '/recovery',
      match: '/recovery',
      glyph: '↺',
    },
    {
      key: 'settings',
      label: 'Settings',
      description: 'Your Nexus account and the diagnostics Onera can report.',
      href: '/settings',
      match: '/settings',
      glyph: '⚙',
    },
  ];
}

/**
 * A display name for a registered installation.
 *
 * `local_games` carries no title — only the adapter and the directory — so the
 * last meaningful path segment is the closest thing to a name Onera has. It is
 * never invented: a root that ends in a separator falls back to the whole path.
 *
 * @param installRoot - The installation directory.
 * @returns The name to show on the card.
 */
export function gameLabel(installRoot: string): string {
  const segments = installRoot.split(/[/\\]+/u).filter((segment) => segment.length > 0);
  return segments[segments.length - 1] ?? installRoot;
}

/** Strip a trailing slash so `/games` and `/games/` are the same address. */
function normalise(pathname: string): string {
  return pathname.length > 1 && pathname.endsWith('/') ? pathname.slice(0, -1) : pathname;
}
