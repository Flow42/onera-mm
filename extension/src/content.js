/**
 * The content script.
 *
 * Its whole job is to show what Onera already has for this mod and offer the
 * actions that make sense for it. It reads no page content and depends on no
 * page structure beyond a single anchor element that it creates itself, so a
 * Nexus redesign cannot break it.
 *
 * Everything shown here comes from the native host by way of the service
 * worker. The script itself decides nothing: which buttons belong on a page is
 * `mod-actions.js`, and whether Onera has the mod is the desktop's answer.
 */

(async () => {
  const CONTAINER_ID = 'onera-actions';
  if (document.getElementById(CONTAINER_ID) !== null) {
    return;
  }

  const container = document.createElement('div');
  container.id = CONTAINER_ID;
  container.style.cssText =
    'position:fixed;right:16px;bottom:16px;z-index:2147483647;display:flex;' +
    'flex-direction:column;align-items:flex-end;gap:6px;' +
    'font-family:system-ui,sans-serif;font-size:13px;';

  const status = document.createElement('div');
  status.textContent = 'Asking Onera…';
  status.style.cssText =
    'padding:4px 10px;border-radius:6px;background:#1b1b1fdd;color:#a1a1aa;' +
    'border:1px solid #33333a;max-width:320px;text-align:right;';

  const row = document.createElement('div');
  row.style.cssText = 'display:flex;gap:8px;flex-wrap:wrap;justify-content:flex-end;';

  container.append(status, row);
  document.body.append(container);

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
      if (response?.ok === true) {
        // The desktop runs the request as soon as it picks it up, so the page's
        // idea of what Onera has is already out of date.
        status.textContent = 'Sent to Onera — running it now.';
        setTimeout(() => void refresh(), 4000);
      }
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

  /**
   * Draw one view returned by `actionsFor`.
   *
   * @param {{ status: string, buttons: { label: string, action: string, primary: boolean }[] }} view
   */
  function render(view) {
    status.textContent = view.status;
    row.replaceChildren();
    for (const { label, action, primary } of view.buttons) {
      const button = document.createElement('button');
      button.textContent = label;
      button.style.cssText =
        'padding:8px 12px;border-radius:6px;cursor:pointer;' +
        (primary
          ? 'border:1px solid transparent;background:#7aa2f7;color:#10101a;font-weight:600;'
          : 'border:1px solid #444;background:#1b1b1f;color:#f4f4f5;');
      button.addEventListener('click', () => void dispatch(action, button));
      row.append(button);
    }
  }

  /** Ask Onera what it has for this mod and redraw. */
  async function refresh() {
    try {
      const response = await chrome.runtime.sendMessage({
        action: 'mod_view',
        url: window.location.href,
      });
      if (response?.ok !== true || response.data?.view === undefined) {
        status.textContent = String(response?.message ?? 'Onera could not be reached.');
        return;
      }
      render(response.data.view);
      // An unreachable host is not a reason to show nothing: the actions still
      // work, and the user is told why the page cannot say what Onera has.
      if (response.data.reachable !== true) {
        status.textContent = String(response.data.message ?? 'Onera could not be reached.');
      }
    } catch {
      status.textContent = 'The Onera extension could not be reached.';
    }
  }

  await refresh();
})();
