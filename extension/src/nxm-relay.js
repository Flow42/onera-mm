/**
 * Carrying a captured download link from the page to Onera.
 *
 * The page-world script (`nxm-intercept.js`) can see the `nxm://` address the
 * site mints but has no way to talk to the extension; this script has the
 * extension's ear but cannot see anything the page's own scripts do. So they
 * are two halves: the page world posts a candidate, this side re-checks the
 * sender — any script on a page can post a message — and the service worker
 * parses it, exactly as it parses every other piece of untrusted input.
 *
 * It runs on every Nexus page and in every frame, because the click that mints
 * the link is not made on the mod page. Pressing "Mod manager download" leads
 * to a page — sometimes a framed one — offering a slow and a premium download,
 * and it is the button there that produces the address.
 *
 * Alongside the relay there is a fallback that matters more than it looks.
 * A page that sets `location.href` cannot be intercepted at all: the property
 * is unforgeable. But a page that is about to navigate to a download link
 * usually *contains* that link already, so this script looks for one and, when
 * it finds one, offers a button. Nothing is handed over without a click:
 * finding a link says the page can start a download, not that the user asked
 * for one.
 */

(() => {
  /** The channel the page-world script posts on. */
  const CHANNEL = 'onera:nxm';

  /** The panel `content.js` publishes on mod pages, when there is one. */
  const PANEL = Symbol.for('onera.panel');

  /**
   * A deliberately loose match, as in the page-world script: what counts as a
   * real download link is decided by the service worker.
   */
  const CANDIDATE = /nxm:(?:\\?\/){2}[A-Za-z0-9][^\s"'<>)]{0,512}/i;

  /** How long the same link is treated as one hand-over rather than two. */
  const REPEAT_WINDOW_MS = 30_000;

  /** The last link sent, so a click and the request behind it count once. */
  let lastSent = { url: '', at: 0 };

  /** This script's own status element, built only when it has something to say. */
  let toast = null;

  /** The mod page's panel, when this frame has one. */
  function panel() {
    const found = window[PANEL];
    return found !== null && typeof found === 'object' ? found : null;
  }

  /**
   * Say what is happening, through the mod page's panel where there is one.
   *
   * @param {string} text - One line for the user.
   */
  function say(text) {
    const target = panel();
    if (target !== null) {
      target.status(text);
      return;
    }
    element().firstChild.textContent = text;
  }

  /**
   * Offer the user a button that hands one link over.
   *
   * @param {string} url - The candidate link, still unvalidated.
   */
  function offerButton(url) {
    const target = panel();
    if (target !== null) {
      target.offer('Send this download to Onera', () => void handOver(url));
      return;
    }
    const container = element();
    const button = document.createElement('button');
    button.textContent = 'Send this download to Onera';
    button.style.cssText =
      'padding:8px 12px;border-radius:6px;cursor:pointer;border:1px solid transparent;' +
      'background:#7aa2f7;color:#10101a;font-weight:600;font:inherit;';
    button.addEventListener('click', () => void handOver(url));
    container.replaceChildren(container.firstChild, button);
  }

  /** This script's own corner of the page, created on first use. */
  function element() {
    if (toast !== null) {
      return toast;
    }
    toast = document.createElement('div');
    toast.id = 'onera-nxm';
    toast.style.cssText =
      'position:fixed;right:16px;bottom:16px;z-index:2147483647;display:flex;' +
      'flex-direction:column;align-items:flex-end;gap:6px;font-family:system-ui,sans-serif;' +
      'font-size:13px;padding:8px 10px;border-radius:8px;background:#1b1b1fee;color:#f4f4f5;' +
      'border:1px solid #33333a;max-width:320px;text-align:right;';
    const line = document.createElement('div');
    line.textContent = 'Onera';
    toast.append(line);
    // Before the body exists there is still a root to hang it on, so a message
    // arriving early is not silently lost.
    (document.body ?? document.documentElement).append(toast);
    return toast;
  }

  /**
   * Send a captured download link to Onera and say what happened.
   *
   * @param {unknown} url - The candidate address, still unvalidated.
   */
  async function handOver(url) {
    if (typeof url !== 'string' || url.length === 0) {
      return;
    }
    const now = Date.now();
    // Several hooks can see one download — the click, and the request that
    // produced the link — and queueing it twice would spend the user's
    // download budget twice.
    if (lastSent.url === url && now - lastSent.at < REPEAT_WINDOW_MS) {
      return;
    }
    lastSent = { url, at: now };

    say('Handing the download to Onera…');
    try {
      const response = await chrome.runtime.sendMessage({ action: 'nxm_download', url });
      if (response?.ok === true) {
        say('Sent to Onera — running it now.');
        setTimeout(() => panel()?.refresh(), 4000);
      } else {
        // A refusal is not a hand-over: the user must be able to press again.
        lastSent = { url: '', at: 0 };
        say(String(response?.message ?? 'Onera could not take that download.'));
      }
    } catch {
      lastSent = { url: '', at: 0 };
      say('The Onera extension could not be reached.');
    }
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window || event.origin !== window.location.origin) {
      return;
    }
    const data = event.data;
    if (data === null || typeof data !== 'object' || data.channel !== CHANNEL) {
      return;
    }
    void handOver(data.url);
  });

  /**
   * A download link the page is already carrying, if it has one.
   *
   * Two places are read, both cheap: the attributes of anchors that spell the
   * scheme, and the text of the page's own inline scripts, which is where a
   * site that navigates from JavaScript keeps the address.
   *
   * @param {boolean} deep - Whether to read the inline scripts as well. The
   *   anchor query is cheap enough to repeat; the scripts are read once.
   * @returns {string | null}
   */
  function linkInPage(deep) {
    const anchor = document.querySelector('a[href^="nxm:" i]');
    if (anchor !== null) {
      return anchor.getAttribute('href');
    }
    if (!deep) {
      return null;
    }
    for (const script of document.scripts) {
      const text = script.textContent;
      if (typeof text !== 'string' || !text.toLowerCase().includes('nxm:')) {
        continue;
      }
      const match = CANDIDATE.exec(text);
      if (match !== null) {
        return match[0].replace(/\\\//g, '/');
      }
    }
    return null;
  }

  /** Look for a link the user could hand over, and offer it once. */
  let offered = null;
  function look(deep) {
    const found = linkInPage(deep);
    if (found === null || found === offered) {
      return;
    }
    offered = found;
    offerButton(found);
  }

  // The link is usually rendered with the page, but the site is an application
  // that fills parts of itself in later, so the document is watched as well as
  // read. The watch costs one selector query per batch of changes, coalesced
  // so that a page rendering itself does not pay for it on every node.
  function watch() {
    look(true);
    let pending = false;
    const observer = new MutationObserver(() => {
      if (pending) {
        return;
      }
      pending = true;
      setTimeout(() => {
        pending = false;
        look(false);
      }, 500);
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
  }

  // Deliberately after the page has loaded rather than as soon as the document
  // parses: on a mod page the panel is published by a content script that runs
  // at idle, and starting first would put a second box on the screen beside it.
  function start() {
    setTimeout(watch, 300);
  }
  if (document.readyState === 'complete') {
    start();
  } else {
    window.addEventListener('load', start, { once: true });
  }
})();
