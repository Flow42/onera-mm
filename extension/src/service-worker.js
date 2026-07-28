/**
 * The extension's service worker.
 *
 * It is the only place that talks to the native host. The popup and the content
 * script send it a message; it validates, forwards and replies. Keeping the
 * native port here means a popup that closes mid-request does not abort the
 * work, and it keeps the host name in one place.
 *
 * Note what this worker never does: it never stores an API key, never receives
 * archive bytes, and never uses `chrome.downloads`. Downloads belong to the
 * native application, which can hash, deduplicate and resume them.
 *
 * @module service-worker
 */

import { identifyModPage } from './page-identity.js';
import { send } from './native.js';

/** Commands the popup and content script may ask for. */
const ACTIONS = Object.freeze({
  ADD: 'add_mod',
  DOWNLOAD: 'download',
  DOWNLOAD_AND_INSTALL: 'download_and_install',
  STATUS: 'status',
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  // `handle` is async, so the listener returns true to keep the channel open.
  handle(message).then(sendResponse, (error) => {
    sendResponse({ ok: false, code: 'internal', message: String(error) });
  });
  return true;
});

/**
 * Handle one message from the popup or content script.
 *
 * @param {unknown} message - The incoming message.
 * @returns {Promise<{ ok: boolean, data?: unknown, code?: string, message?: string }>}
 */
export async function handle(message) {
  if (message === null || typeof message !== 'object') {
    return { ok: false, code: 'malformed', message: 'Empty request.' };
  }
  const { action, url, fileId } = /** @type {Record<string, unknown>} */ (message);

  if (action === ACTIONS.STATUS) {
    return send('status');
  }

  if (
    action !== ACTIONS.ADD &&
    action !== ACTIONS.DOWNLOAD &&
    action !== ACTIONS.DOWNLOAD_AND_INSTALL
  ) {
    return { ok: false, code: 'malformed', message: 'Unknown action.' };
  }

  // The identity is re-derived here rather than trusted from the sender: a
  // content script runs in a page's process and its messages are not authority.
  const identity = typeof url === 'string' ? identifyModPage(url) : null;
  if (identity === null) {
    return { ok: false, code: 'malformed', message: 'This is not a Nexus Mods mod page.' };
  }

  /** @type {Record<string, unknown>} */
  const payload = { game_domain: identity.gameDomain, mod_id: identity.modId };
  if (action !== ACTIONS.ADD) {
    payload.file_id = typeof fileId === 'string' && fileId.length > 0 ? fileId : null;
  }
  return send(action, payload);
}

export { ACTIONS };
