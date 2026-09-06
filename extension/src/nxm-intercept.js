/**
 * Catching the download link the page mints for a mod manager.
 *
 * This is the one script Onera runs in the page's own world, and it exists for
 * a reason nothing else can solve: "Mod manager download" ends in a navigation
 * to a `nxm://` address, and a navigation to a scheme the browser does not own
 * is invisible to an extension. Whatever the page does with that address —
 * follow a link, open a window, assign to `location` — the address itself first
 * arrives over the network, so the reliable place to see it is the response
 * that carried it.
 *
 * Three rules keep this honest:
 *
 * * it reads, and changes nothing. Every hook calls through to the original and
 *   returns exactly what it returned, so the page behaves the same whether or
 *   not Onera is installed. The one exception is a click on a link the page
 *   itself marked `nxm:`, which is cancelled because Onera has taken it;
 * * it depends on no page structure — not a class, not a button, not a route —
 *   only on the `nxm:` scheme, which every mod manager on every platform
 *   consumes and which therefore cannot quietly change;
 * * it decides nothing. It forwards a candidate string to the extension, which
 *   parses and validates it as untrusted input, exactly as it treats a page URL.
 *
 * @module nxm-intercept
 */

(() => {
  /** Marks the message as this script's, so the content script can filter. */
  const CHANNEL = 'onera:nxm';

  /** Longest candidate forwarded. A download link is far shorter than this. */
  const MAX_URL_LENGTH = 2048;

  /**
   * A first, deliberately loose match. The authoritative parse — which decides
   * what is a real download link — lives in `nxm.js`, on the extension's side
   * of the boundary.
   */
  const CANDIDATE = /nxm:(?:\\?\/){2}[A-Za-z0-9][^\s"'<>)]{0,512}/i;

  // A page that navigates and comes back, or an extension that reloads, must
  // not end up with two sets of hooks calling each other.
  if (window[Symbol.for('onera.nxm.installed')] === true) {
    return;
  }
  window[Symbol.for('onera.nxm.installed')] = true;

  /**
   * Hand one candidate address to the extension.
   *
   * @param {unknown} candidate - Something that may be a download link.
   */
  function offer(candidate) {
    if (typeof candidate !== 'string' || candidate.length > MAX_URL_LENGTH) {
      return;
    }
    if (!candidate.slice(0, 8).toLowerCase().startsWith('nxm:')) {
      return;
    }
    window.postMessage({ channel: CHANNEL, url: candidate }, window.location.origin);
  }

  /**
   * Search a response body for a download link and offer what it finds.
   *
   * @param {unknown} text - The body, when it was readable.
   */
  function offerFromText(text) {
    if (typeof text !== 'string' || !text.toLowerCase().includes('nxm:')) {
      return;
    }
    const match = CANDIDATE.exec(text);
    if (match !== null) {
      // JSON bodies escape their slashes; the extension's parser normalises the
      // rest, but this much is needed for the address to survive the trip.
      offer(match[0].replace(/\\\//g, '/'));
    }
  }

  // 1. A link the page renders with the scheme on it. Cancelled, because
  //    following it would ask the operating system to open some other mod
  //    manager for a download Onera has already taken.
  document.addEventListener(
    'click',
    (event) => {
      const target = event.target;
      if (!(target instanceof Element)) {
        return;
      }
      const anchor = target.closest('a[href^="nxm:" i]');
      if (anchor instanceof HTMLAnchorElement) {
        offer(anchor.href);
        event.preventDefault();
      }
    },
    true,
  );

  // 2. The address arriving over the network. This is the case that actually
  //    fires on the current site: the page asks for a download link and then
  //    assigns it to `location`, which nothing can observe.
  const nativeFetch = window.fetch;
  if (typeof nativeFetch === 'function') {
    window.fetch = function fetchWithNxmWatch(...args) {
      const result = nativeFetch.apply(this, args);
      // The page's own promise is returned untouched; the copy is read on the
      // side, and a failure to read it is not the page's problem.
      void result.then(
        (response) => {
          try {
            void response
              .clone()
              .text()
              .then(offerFromText, () => {});
          } catch {
            /* An unclonable or already-consumed body is simply not searched. */
          }
        },
        () => {},
      );
      return result;
    };
  }

  const nativeSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.send = function sendWithNxmWatch(...args) {
    this.addEventListener('load', () => {
      try {
        // `responseText` throws for a response type that has none, which is a
        // response that cannot contain a link anyway.
        offerFromText(this.responseText);
      } catch {
        /* ignored, deliberately */
      }
    });
    return nativeSend.apply(this, args);
  };

  // 3. The remaining ways a page can send the browser somewhere by name.
  //    `location.href = …` is not among them — it cannot be intercepted — which
  //    is precisely why case 2 exists.
  const nativeOpen = window.open;
  if (typeof nativeOpen === 'function') {
    window.open = function openWithNxmWatch(url, ...rest) {
      offer(url);
      return nativeOpen.call(this, url, ...rest);
    };
  }

  for (const name of ['assign', 'replace']) {
    const native = Location.prototype[name];
    if (typeof native !== 'function') {
      continue;
    }
    Location.prototype[name] = function navigateWithNxmWatch(url, ...rest) {
      offer(url);
      return native.call(this, url, ...rest);
    };
  }
})();
