/**
 * The mod card is the one place a user reads dates and versions side by side,
 * so these tests pin what it may and may not say: never a parsed version, never
 * an invented date, and never a link Onera would refuse to open.
 */
import { describe, expect, it } from 'vitest';
import { formatOptionalDate, initialsOf, toCard } from './mod-card';
import type { InstalledMod } from './types';

function mod(overrides: Partial<InstalledMod> = {}): InstalledMod {
  return {
    installation_id: 'i-1',
    mod_id: 'm-1',
    name: 'Better Vehicle Handling',
    author: 'someone',
    version: '1.4.1',
    installed_at: '2026-03-04T10:00:00Z',
    published_at: '2026-02-01T00:00:00Z',
    update_available: false,
    latest_version: null,
    latest_published_at: null,
    game_slug: 'cyberpunk2077',
    provider_mod_id: '107',
    thumbnail_url: null,
    ...overrides,
  };
}

describe('toCard', () => {
  it('shows the version tag exactly as published', () => {
    for (const version of ['1.4.1', 'v2 FINAL', '2026-03 build', '']) {
      expect(toCard(mod({ version })).version).toBe(version);
    }
  });

  it('reports no update and no newer version when none is available', () => {
    const card = toCard(mod({ update_available: false, latest_version: '9.9' }));
    expect(card.updateAvailable).toBe(false);
    // The backend only names a latest version when it is genuinely newer, and
    // the card must not advertise one it was told not to.
    expect(card.latestVersion).toBeNull();
  });

  it('names the newer version when an update is available', () => {
    const card = toCard(
      mod({
        update_available: true,
        latest_version: '2.0',
        latest_published_at: '2026-08-09T00:00:00Z',
      }),
    );
    expect(card.updateAvailable).toBe(true);
    expect(card.latestVersion).toBe('2.0');
    expect(card.latestPublished).not.toBeNull();
  });

  it('falls back to the installed release date when nothing newer is known', () => {
    const card = toCard(mod({ latest_published_at: null, published_at: '2026-02-01T00:00:00Z' }));
    expect(card.latestPublished).toBe(formatOptionalDate('2026-02-01T00:00:00Z'));
  });

  it('says nothing about a publication date the provider never gave', () => {
    const card = toCard(mod({ latest_published_at: null, published_at: null }));
    expect(card.latestPublished).toBeNull();
  });

  it('offers a provider link only for identifiers Onera would open', () => {
    expect(toCard(mod()).canOpenPage).toBe(true);
    for (const slug of ['', '../etc', 'a b', 'x'.repeat(65), 'game/../..']) {
      expect(toCard(mod({ game_slug: slug })).canOpenPage).toBe(false);
    }
    expect(toCard(mod({ provider_mod_id: '10 7' })).canOpenPage).toBe(false);
  });
});

describe('formatOptionalDate', () => {
  it('returns null for a missing date', () => {
    expect(formatOptionalDate(null)).toBeNull();
    expect(formatOptionalDate(undefined)).toBeNull();
    expect(formatOptionalDate('')).toBeNull();
  });

  it('shows an unreadable timestamp rather than inventing one', () => {
    expect(formatOptionalDate('whenever')).toBe('whenever');
  });

  it('formats a real timestamp as a date', () => {
    const formatted = formatOptionalDate('2026-03-04T10:00:00Z');
    expect(formatted).not.toBeNull();
    expect(formatted).toContain('2026');
  });
});

describe('initialsOf', () => {
  it('takes the first letter of the first two words', () => {
    expect(initialsOf('Better Vehicle Handling')).toBe('BV');
    expect(initialsOf('Skyrim')).toBe('S');
    expect(initialsOf('hd_texture-pack')).toBe('HT');
  });

  it('skips leading punctuation to find a real character', () => {
    expect(initialsOf('[WIP] Nova City')).toBe('WN');
    expect(initialsOf('!!! 4K Textures')).toBe('4T');
  });

  it('falls back to a placeholder for a name with no letters', () => {
    expect(initialsOf('')).toBe('?');
    expect(initialsOf('...')).toBe('?');
  });
});
