/**
 * Choosing what to offer for the mod on the page.
 *
 * The buttons a mod page should carry depend entirely on what Onera already
 * has, and that decision is made here rather than in the content script so it
 * can be tested without a browser. The script's job is reduced to drawing
 * whatever this returns.
 *
 * One rule shapes every case below: a mod Onera already has is never offered a
 * plain "download" as the obvious action. Re-fetching an archive that is
 * already in the store wastes the user's download budget and tells them nothing
 * they did not know; what they want to hear is that it is already there.
 *
 * @module mod-actions
 */

/** Actions the service worker accepts for a mod page. */
export const ACTION = Object.freeze({
  ADD: 'add_mod',
  DOWNLOAD: 'download',
  DOWNLOAD_AND_INSTALL: 'download_and_install',
});

/**
 * @typedef {object} ModState
 * @property {string | null} [name] - Cached display name.
 * @property {boolean} [game_registered] - Whether the game is registered.
 * @property {boolean} [downloaded] - Whether an archive is already stored.
 * @property {boolean} [installed] - Whether a copy is installed.
 * @property {string | null} [installed_version] - Installed version, verbatim.
 * @property {boolean} [update_available] - Whether a newer release exists.
 * @property {string | null} [latest_version] - Newest version, verbatim.
 */

/**
 * @typedef {object} ModActionView
 * @property {string} status - One line describing what Onera has.
 * @property {'none' | 'downloaded' | 'installed' | 'update'} kind - Which case this is.
 * @property {{ label: string, action: string, primary: boolean }[]} buttons - What to offer.
 */

/**
 * Decide what to show for one mod.
 *
 * A missing or unreadable state is treated as "Onera knows nothing", which is
 * the same case as a genuinely new mod: the user still gets every action, and
 * the worst outcome is a duplicate request the desktop deduplicates anyway.
 *
 * @param {ModState | null | undefined} state - The host's answer, if it replied.
 * @returns {ModActionView}
 */
export function actionsFor(state) {
  const known = state ?? {};
  const installable = known.game_registered === true;

  if (known.installed === true && known.update_available === true) {
    return {
      kind: 'update',
      status: `Installed: ${versionText(known.installed_version)} — update to ${versionText(
        known.latest_version,
      )} available`,
      buttons: [
        {
          label: `Update to ${versionText(known.latest_version)}`,
          // An update is an install of a newer file, so it takes the same path
          // and gets the same conflict preview. Nothing is special-cased.
          action: installable ? ACTION.DOWNLOAD_AND_INSTALL : ACTION.DOWNLOAD,
          primary: true,
        },
        { label: 'Download only', action: ACTION.DOWNLOAD, primary: false },
      ],
    };
  }

  if (known.installed === true) {
    return {
      kind: 'installed',
      status: `Already installed: ${versionText(known.installed_version)}`,
      // Re-installing over itself is a real thing to want after a verify
      // failure, so it stays available — just not as the obvious button.
      buttons: [{ label: 'Reinstall', action: ACTION.DOWNLOAD_AND_INSTALL, primary: false }],
    };
  }

  if (known.downloaded === true) {
    return {
      kind: 'downloaded',
      status: 'Already downloaded — not installed yet',
      buttons: [
        {
          label: 'Install',
          action: ACTION.DOWNLOAD_AND_INSTALL,
          primary: true,
        },
      ],
    };
  }

  return {
    kind: 'none',
    status: installable ? 'Not in Onera yet' : 'Not in Onera yet — no matching game registered',
    buttons: [
      {
        label: 'Download and install',
        action: ACTION.DOWNLOAD_AND_INSTALL,
        primary: installable,
      },
      { label: 'Download', action: ACTION.DOWNLOAD, primary: false },
      { label: 'Add to Onera', action: ACTION.ADD, primary: false },
    ],
  };
}

/**
 * The transfer a download that arrived from outside Onera should become.
 *
 * A download link the user minted on the mod page says which file, not what to
 * do with it. Installing is what a mod manager is for, so that is the answer
 * wherever Onera has a game to install into — and a plain download where it has
 * not, because queueing an install with nowhere to put it would only park the
 * request until the user came back to answer for it.
 *
 * @param {ModState | null | undefined} state - The host's answer, if it replied.
 * @returns {string} One of [`ACTION`]'s transfer actions.
 */
export function transferActionFor(state) {
  return state?.game_registered === true ? ACTION.DOWNLOAD_AND_INSTALL : ACTION.DOWNLOAD;
}

/**
 * Render a version for display.
 *
 * Versions are provider strings and are never parsed or compared here; this
 * only covers the case where the provider published none.
 *
 * @param {string | null | undefined} version - The version, verbatim.
 * @returns {string}
 */
function versionText(version) {
  return typeof version === 'string' && version.length > 0 ? version : 'an unnamed version';
}
