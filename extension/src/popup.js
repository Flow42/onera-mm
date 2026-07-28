/**
 * The popup.
 *
 * Shows whether Onera is reachable and authenticated, and offers the three
 * actions when the active tab is a mod page.
 */

import { identifyModPage } from './page-identity.js';

const statusEl = /** @type {HTMLElement} */ (document.getElementById('status'));
const actionsEl = /** @type {HTMLElement} */ (document.getElementById('actions'));

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

async function main() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const identity = typeof tab?.url === 'string' ? identifyModPage(tab.url) : null;

  const status = await chrome.runtime.sendMessage({ action: 'status' });
  if (status?.ok !== true) {
    setStatus(String(status?.message ?? 'Could not reach Onera.'), true);
    return;
  }
  if (status.data?.authenticated !== true) {
    setStatus('Onera is running, but no Nexus API key is set. Open Onera to finish setup.', true);
    return;
  }
  if (identity === null) {
    setStatus('Open a Nexus Mods mod page to send it to Onera.');
    return;
  }

  setStatus(`Ready: ${identity.gameDomain} mod ${identity.modId}`);
  actionsEl.hidden = false;

  for (const button of actionsEl.querySelectorAll('button')) {
    button.addEventListener('click', async () => {
      button.disabled = true;
      const response = await chrome.runtime.sendMessage({
        action: button.dataset.action,
        url: tab.url,
      });
      if (response?.ok === true) {
        setStatus('Sent to Onera.');
      } else {
        setStatus(String(response?.message ?? 'Onera reported an error.'), true);
      }
      button.disabled = false;
    });
  }
}

void main().catch((error) => setStatus(String(error), true));
