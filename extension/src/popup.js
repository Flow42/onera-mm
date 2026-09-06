/**
 * The popup.
 *
 * Shows three things and offers one decision: whether the Onera window is
 * running, whether it is authenticated, and what it already has for the mod on
 * the current tab. The actions offered are the same ones the page itself shows,
 * decided in one place by the service worker.
 */

import { identifyModPage } from './page-identity.js';

const statusEl = /** @type {HTMLElement} */ (document.getElementById('status'));
const actionsEl = /** @type {HTMLElement} */ (document.getElementById('actions'));
const appStateEl = /** @type {HTMLElement} */ (document.getElementById('app-state'));
const appDotEl = /** @type {HTMLElement} */ (document.getElementById('app-dot'));
const launchEl = /** @type {HTMLButtonElement} */ (document.getElementById('launch'));

/**
 * Render the current state.
 *
 * @param {string} text - Message to show.
 * @param {boolean} [isError] - Whether to style it as an error.
 */
function setStatus(text, isError = false) {
  statusEl.textContent = text;
  statusEl.classList.toggle('error', isError);
}

/**
 * Show whether the desktop application is running.
 *
 * The window running is not a precondition for anything the popup offers — a
 * queued request is durable and runs when the window next opens — so this is
 * reported rather than enforced.
 *
 * @param {boolean} running - Whether a live heartbeat was found.
 */
function setAppState(running) {
  appStateEl.textContent = running ? 'Onera is running' : 'Onera is not running';
  appDotEl.classList.toggle('running', running);
  appDotEl.classList.toggle('stopped', !running);
  launchEl.hidden = running;
}

async function main() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const identity = typeof tab?.url === 'string' ? identifyModPage(tab.url) : null;

  const status = await chrome.runtime.sendMessage({ action: 'status' });
  if (status?.ok !== true) {
    setAppState(false);
    setStatus(String(status?.message ?? 'Could not reach Onera.'), true);
    return;
  }
  setAppState(status.data?.desktop_running === true);

  launchEl.addEventListener('click', async () => {
    launchEl.disabled = true;
    launchEl.textContent = 'Opening…';
    const started = await chrome.runtime.sendMessage({ action: 'ensure_app' });
    if (started?.ok !== true) {
      setStatus(String(started?.message ?? 'Onera could not be started.'), true);
      launchEl.disabled = false;
      launchEl.textContent = 'Open Onera';
      return;
    }
    // The window writes its first heartbeat a moment after it starts, so the
    // popup reports what it did rather than pretending to know it succeeded.
    launchEl.textContent = 'Opening…';
  });

  if (status.data?.authenticated !== true) {
    setStatus('Onera is running, but no Nexus API key is set. Open Onera to finish setup.', true);
    return;
  }
  if (identity === null) {
    setStatus('Open a Nexus Mods mod page to send it to Onera.');
    return;
  }

  setStatus('Asking Onera about this mod…');
  const view = await chrome.runtime.sendMessage({ action: 'mod_view', url: tab.url });
  if (view?.ok !== true || view.data?.view === undefined) {
    setStatus(String(view?.message ?? 'Onera could not be reached.'), true);
    return;
  }
  render(view.data.view, tab.url);
  if (view.data.reachable !== true) {
    setStatus(String(view.data.message ?? 'Onera could not be reached.'), true);
  }
}

/**
 * Draw the actions the service worker chose for this mod.
 *
 * @param {{ status: string, buttons: { label: string, action: string, primary: boolean }[] }} view
 * @param {string} url - The tab's URL, re-derived by the worker before use.
 */
function render(view, url) {
  setStatus(view.status);
  actionsEl.replaceChildren();
  actionsEl.hidden = false;
  for (const { label, action, primary } of view.buttons) {
    const button = document.createElement('button');
    button.textContent = label;
    button.classList.toggle('primary', primary);
    button.addEventListener('click', async () => {
      button.disabled = true;
      const response = await chrome.runtime.sendMessage({ action, url });
      if (response?.ok === true) {
        setStatus('Sent to Onera.');
        // The request queues either way; this only reports what happened to the
        // window, which is the part the user cannot see from the browser.
        if (response.desktop?.launched === true) setAppState(false);
      } else {
        setStatus(String(response?.message ?? 'Onera reported an error.'), true);
      }
      button.disabled = false;
    });
    actionsEl.append(button);
  }
}

void main().catch((error) => setStatus(String(error), true));
