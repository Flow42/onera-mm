/**
 * Extracting a mod's identity from a Nexus Mods URL.
 *
 * This module deliberately reads *only the URL*. Nexus rewrites its mod pages
 * regularly, and an extension that scraped titles, file tables or download
 * buttons would break every time. The URL carries the two things that are
 * stable and sufficient — the game domain and the mod id — and the native
 * application asks the API for everything else.
 *
 * @module page-identity
 */

/** Hosts whose URLs this extension will read. */
export const ALLOWED_HOSTS = new Set(['www.nexusmods.com', 'nexusmods.com']);

/**
 * The shape of a mod page URL: `/<game-domain>/mods/<id>`, with anything
 * after it (tabs, query strings, fragments) ignored.
 */
const MOD_PATH = /^\/([a-z0-9]+(?:[a-z0-9-]*[a-z0-9])?)\/mods\/(\d+)(?:\/.*)?$/i;

/** Longest identifier accepted, matching the native host's own limit. */
const MAX_ID_LENGTH = 64;

/**
 * Extract `{ gameDomain, modId }` from a URL, or `null` if it is not a mod page.
 *
 * @param {string} rawUrl - The page URL.
 * @returns {{ gameDomain: string, modId: string } | null}
 */
export function identifyModPage(rawUrl) {
  let url;
  try {
    url = new URL(rawUrl);
  } catch {
    return null;
  }
  if (url.protocol !== 'https:' || !ALLOWED_HOSTS.has(url.hostname)) {
    return null;
  }

  const match = MOD_PATH.exec(url.pathname);
  if (match === null) {
    return null;
  }

  const [, gameDomain, modId] = match;
  if (gameDomain.length > MAX_ID_LENGTH || modId.length > MAX_ID_LENGTH) {
    return null;
  }
  // Normalising the domain here means the native host receives exactly one
  // spelling of each game, whatever case the URL used.
  return { gameDomain: gameDomain.toLowerCase(), modId };
}

/**
 * Whether a URL is a mod page this extension can act on.
 *
 * @param {string} rawUrl - The page URL.
 * @returns {boolean}
 */
export function isModPage(rawUrl) {
  return identifyModPage(rawUrl) !== null;
}
