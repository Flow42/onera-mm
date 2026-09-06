/**
 * Reading a `nxm://` download link.
 *
 * Pressing "Mod manager download" on a mod page makes Nexus mint one of these
 * and hand it to whatever mod manager the operating system has registered. It
 * is the only way an account without a paid tier can obtain a download
 * location: the API issues one directly to premium members and refuses everyone
 * else, but the same endpoint accepts the nonce this link carries.
 *
 * The link is a contract in a way page markup is not — every mod manager on
 * every platform consumes this exact shape — so reading it costs nothing in
 * fragility, unlike scraping the button that produced it. What this module will
 * not do is trust it: it arrives from a page, so every field is checked before
 * it becomes a request, and anything unfamiliar is dropped rather than passed
 * along.
 *
 * @module nxm
 */

/**
 * The shape of a download link: `nxm://<game>/mods/<id>/files/<file id>`.
 *
 * The file id here is the *site's* id for the file — the number in its own
 * URLs — which is not the identifier the API keys on. The native host knows
 * both and matches on either, so the number is passed through as it arrives.
 */
const NXM_PATH = /^\/mods\/(\d+)\/files\/(\d+)$/;

/** The game domain, spelled as it is in a mod page's URL. */
const GAME_DOMAIN = /^[a-z0-9]+(?:[a-z0-9-]*[a-z0-9])?$/i;

/** Longest identifier accepted, matching the native host's own limit. */
const MAX_ID_LENGTH = 64;

/** The nonce's alphabet and length, as Nexus issues it. */
const KEY = /^[A-Za-z0-9_-]{1,128}$/;

/**
 * Finds a download link inside a larger body of text.
 *
 * The escapes are not decoration: a JSON body spells the address with its
 * slashes escaped, and the link is only recognisable if the pattern allows for
 * that spelling as well as the plain one.
 */
const NXM_IN_TEXT = /nxm:(?:\\?\/){2}[A-Za-z0-9][^\s"'<>)]*/i;

/**
 * @typedef {object} NxmDownload
 * @property {string} gameDomain - Provider game slug.
 * @property {string} modId - Mod id, as it appears in the page URL.
 * @property {string} fileId - The site's id for the file.
 * @property {{ key: string, expires: number } | null} grant - The permission
 *   the site minted, when the link carries one. Premium links may not.
 */

/**
 * Read a download link.
 *
 * @param {unknown} raw - The candidate link.
 * @returns {NxmDownload | null} `null` for anything that is not one.
 */
export function parseNxmUrl(raw) {
  if (typeof raw !== 'string' || raw.length > 2048) {
    return null;
  }
  let url;
  try {
    url = new URL(raw);
  } catch {
    return null;
  }
  if (url.protocol !== 'nxm:') {
    return null;
  }

  const gameDomain = url.hostname.toLowerCase();
  if (!GAME_DOMAIN.test(gameDomain) || gameDomain.length > MAX_ID_LENGTH) {
    return null;
  }
  const match = NXM_PATH.exec(url.pathname);
  if (match === null) {
    return null;
  }
  const [, modId, fileId] = match;
  if (modId.length > MAX_ID_LENGTH || fileId.length > MAX_ID_LENGTH) {
    return null;
  }

  return { gameDomain, modId, fileId, grant: readGrant(url.searchParams) };
}

/**
 * Read the nonce, if the link carries a usable one.
 *
 * A half-formed grant is treated as no grant at all rather than as an error:
 * the request is still a real request, and a premium account resolves it
 * without one. What is never done is passing a malformed nonce on in the hope
 * that the provider makes sense of it.
 *
 * @param {URLSearchParams} params - The link's query.
 * @returns {{ key: string, expires: number } | null}
 */
function readGrant(params) {
  const key = params.get('key');
  const expires = params.get('expires');
  if (key === null || expires === null || !KEY.test(key) || !/^\d{1,15}$/.test(expires)) {
    return null;
  }
  return { key, expires: Number(expires) };
}

/**
 * Find a download link inside arbitrary text, such as a JSON response body.
 *
 * The site mints the link over the network and then navigates to it, and a
 * navigation to a scheme the browser does not own is not something an extension
 * can observe. Recognising the link in the answer that carried it is what makes
 * the interception work whatever the page does with it next.
 *
 * @param {unknown} text - Text to search.
 * @returns {string | null} The first link found, or `null`.
 */
export function findNxmUrl(text) {
  if (typeof text !== 'string' || text.length === 0) {
    return null;
  }
  const match = NXM_IN_TEXT.exec(text);
  if (match === null) {
    return null;
  }
  // JSON escapes its slashes sometimes, and a body may end the URL with an
  // escaped quote; parsing decides what is real.
  const candidate = match[0].replace(/\\\//g, '/').replace(/\\+$/, '');
  return parseNxmUrl(candidate) === null ? null : candidate;
}
