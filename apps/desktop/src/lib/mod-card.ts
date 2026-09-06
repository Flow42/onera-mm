/**
 * View-model for the installed-mod cards.
 *
 * Pure, so the decisions a card makes can be tested without a DOM: what its
 * dates say, whether it claims an update is available, and whether it can offer
 * a link back to the provider.
 *
 * The version rule from the core holds here too. A version string is displayed
 * exactly as the author published it and is never parsed; whether something
 * newer exists is a question the backend already answered by comparing
 * publication dates, and this module only reports that answer.
 */

import type { InstalledMod } from './types';

/** One card, ready to render. */
export interface ModCard {
  installationId: string;
  modId: string;
  name: string;
  author: string | null;
  /** The version tag, exactly as published. */
  version: string;
  /** Absolute date the mod was installed. */
  installed: string;
  /** Absolute date the newest known release was published, if any. */
  latestPublished: string | null;
  /** Newest version tag, when one is newer than the installed release. */
  latestVersion: string | null;
  updateAvailable: boolean;
  /** Initials shown while artwork loads, or in place of artwork it lacks. */
  initials: string;
  /** Whether the provider page can be opened for this mod. */
  canOpenPage: boolean;
  gameSlug: string;
  providerModId: string;
}

/**
 * Build the card for one installed mod.
 *
 * @param mod - A row from `installed_mods` or `check_updates`.
 * @returns The card to render.
 */
export function toCard(mod: InstalledMod): ModCard {
  return {
    installationId: mod.installation_id,
    modId: mod.mod_id,
    name: mod.name,
    author: mod.author ?? null,
    version: mod.version,
    installed: formatDate(mod.installed_at),
    // Falls back to the installed release's own date: a mod nobody has checked
    // for updates still has a "latest version" — the one that is installed.
    latestPublished: formatOptionalDate(mod.latest_published_at ?? mod.published_at),
    latestVersion: mod.update_available ? mod.latest_version : null,
    updateAvailable: mod.update_available,
    initials: initialsOf(mod.name),
    canOpenPage: isOpenable(mod.game_slug) && isOpenable(mod.provider_mod_id),
    gameSlug: mod.game_slug,
    providerModId: mod.provider_mod_id,
  };
}

/**
 * Format a timestamp for display, in the user's own locale and time zone.
 *
 * @param iso - An RFC 3339 timestamp from the backend.
 * @returns A date, or the raw value when it cannot be read.
 */
export function formatDate(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) {
    // Better to show something odd than to invent a date or an em dash where a
    // real timestamp was meant to be.
    return iso;
  }
  return at.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

/**
 * Format a timestamp that the provider may never have supplied.
 *
 * @param iso - A timestamp, or null.
 * @returns The formatted date, or null when there was none.
 */
export function formatOptionalDate(iso: string | null | undefined): string | null {
  return typeof iso === 'string' && iso.length > 0 ? formatDate(iso) : null;
}

/**
 * Up to two initials for a mod, used when it has no artwork.
 *
 * @param name - The mod's display name.
 * @returns One or two uppercase characters, or `?` for a nameless mod.
 */
export function initialsOf(name: string): string {
  const words = name
    .split(/[\s_-]+/u)
    .map((word) => [...word].find((character) => /\p{L}|\p{N}/u.test(character)))
    .filter((character): character is string => character !== undefined);
  if (words.length === 0) {
    return '?';
  }
  return words.slice(0, 2).join('').toUpperCase();
}

/**
 * Whether an identifier is safe to build a provider URL from.
 *
 * The same shape the native host and the Tauri command accept. Checking it here
 * too means a card never offers a link that the backend will refuse.
 *
 * @param value - A game slug or provider mod id.
 * @returns Whether it can appear in a page address.
 */
function isOpenable(value: string | null | undefined): boolean {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= 64 &&
    /^[A-Za-z0-9_-]+$/u.test(value)
  );
}
