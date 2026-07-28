/**
 * The content script.
 *
 * Its whole job is to add three buttons and report the page URL. It reads no
 * page content and depends on no page structure beyond a single anchor element
 * that it creates itself, so a Nexus redesign cannot break it.
 */

(() => {
  const CONTAINER_ID = 'onera-actions';
  if (document.getElementById(CONTAINER_ID) !== null) {
    return;
  }

  /**
   * Ask the service worker to act, and reflect the outcome on the button.
   *
   * @param {string} action - One of the service worker's actions.
   * @param {HTMLButtonElement} button - The button that was pressed.
   */
  async function dispatch(action, button) {
    const original = button.textContent;
    button.disabled = true;
    button.textContent = 'Working…';
    try {
      const response = await chrome.runtime.sendMessage({ action, url: window.location.href });
      button.textContent = response?.ok === true ? 'Sent to Onera' : 'Failed';
      button.title = response?.ok === true ? '' : String(response?.message ?? '');
    } catch {
      button.textContent = 'Failed';
      button.title = 'Could not reach the Onera extension.';
    } finally {
      setTimeout(() => {
        button.disabled = false;
        button.textContent = original;
      }, 2500);
    }
  }

  const container = document.createElement('div');
  container.id = CONTAINER_ID;
  container.style.cssText =
    'position:fixed;right:16px;bottom:16px;z-index:2147483647;display:flex;gap:8px;' +
    'font-family:system-ui,sans-serif;font-size:13px;';

  for (const [label, action] of [
    ['Add to Onera', 'add_mod'],
    ['Download', 'download'],
    ['Download and install', 'download_and_install'],
  ]) {
    const button = document.createElement('button');
    button.textContent = label;
    button.style.cssText =
      'padding:8px 12px;border-radius:6px;border:1px solid #444;background:#1b1b1f;' +
      'color:#f4f4f5;cursor:pointer;';
    button.addEventListener('click', () => void dispatch(action, button));
    container.append(button);
  }

  document.body.append(container);
})();
