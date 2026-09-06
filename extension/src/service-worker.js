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

import { actionsFor } from './mod-actions.js';
import { canonicalModUrl, identifyModPage } from './page-identity.js';
import { send } from './native.js';

/** Commands the popup and content script may ask for. */
const ACTIONS = Object.freeze({
  ADD: 'add_mod',
  DOWNLOAD: 'download',
  DOWNLOAD_AND_INSTALL: 'download_and_install',
  STATUS: 'status',
  MOD_STATE: 'mod_state',
  MOD_VIEW: 'mod_view',
  APP_STATE: 'app_state',
  ENSURE_APP: 'ensure_app',
});

/** Actions that queue work for the desktop, and therefore need it running. */
const TRANSFER_ACTIONS = new Set([ACTIONS.DOWNLOAD, ACTIONS.DOWNLOAD_AND_INSTALL]);

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
  if (action === ACTIONS.APP_STATE) {
    return send('app_state');
  }
  if (action === ACTIONS.ENSURE_APP) {
    return ensureDesktop();
  }

  if (
    action !== ACTIONS.ADD &&
    action !== ACTIONS.DOWNLOAD &&
    action !== ACTIONS.DOWNLOAD_AND_INSTALL &&
    action !== ACTIONS.MOD_STATE &&
    action !== ACTIONS.MOD_VIEW
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

  if (action === ACTIONS.MOD_STATE) {
    return send(action, payload);
  }

  // The content script runs as a classic script and cannot import a module, so
  // the decision about which buttons a page should carry is made here and sent
  // to it already made. That also keeps the rule in one testable place.
  if (action === ACTIONS.MOD_VIEW) {
    const state = await send(ACTIONS.MOD_STATE, payload);
    return {
      ok: true,
      data: {
        view: actionsFor(state.ok === true ? state.data : null),
        reachable: state.ok === true,
        message: state.ok === true ? null : state.message,
      },
    };
  }

  if (action !== ACTIONS.ADD) {
    payload.file_id = typeof fileId === 'string' && fileId.length > 0 ? fileId : null;
    // Rebuilt from the identity, never forwarded from the tab: what gets stored
    // should be the mod's page, without whatever tab or query string the user
    // happened to be on.
    payload.page_url = canonicalModUrl(identity);
  }

  // A queued transfer is only run by the desktop application, so it is started
  // first. Failing to start it does not cancel the request — the work is
  // durable in Onera's inbox and runs whenever the window next opens — so the
  // outcome is reported alongside the queued request rather than instead of it.
  const desktop = TRANSFER_ACTIONS.has(action) ? await ensureDesktop() : null;

  const result = await send(action, payload);
  if (result.ok === true && desktop !== null) {
    return { ...result, desktop: desktop.data ?? null };
  }
  return result;
}

/**
 * Make sure the desktop application is running, starting it if it is not.
 *
 * The launch is fire-and-forget: a window takes seconds to appear and longer to
 * write its first heartbeat, and blocking a button click on that would make
 * Onera feel broken. The request the user made is durable either way.
 *
 * @returns {Promise<{ ok: boolean, data?: unknown, code?: string, message?: string }>}
 */
export async function ensureDesktop() {
  const state = await send('app_state');
  if (state.ok !== true) {
    return state;
  }
  const data = /** @type {Record<string, unknown>} */ (state.data ?? {});
  if (data.running === true) {
    return { ok: true, data: { running: true, launched: false } };
  }
  if (data.launchable !== true) {
    return {
      ok: false,
      code: 'not_found',
      message: 'Onera is installed as a browser connector only; its window could not be found.',
    };
  }
  return send('launch_app');
}

export { ACTIONS };
