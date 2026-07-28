/**
 * The extension must extract only stable page identity, and must refuse
 * anything else. These tests are the specification of what "stable" means.
 */
import { describe, expect, it } from 'vitest';
// @ts-expect-error - the extension ships plain JS with JSDoc types.
import { identifyModPage, isModPage } from '../../extension/src/page-identity.js';

describe('identifyModPage', () => {
  it('reads the game domain and mod id from a mod page URL', () => {
    expect(identifyModPage('https://www.nexusmods.com/cyberpunk2077/mods/107')).toEqual({
      gameDomain: 'cyberpunk2077',
      modId: '107',
    });
  });

  it('ignores tabs, query strings and fragments', () => {
    for (const url of [
      'https://www.nexusmods.com/cyberpunk2077/mods/107?tab=files',
      'https://www.nexusmods.com/cyberpunk2077/mods/107/',
      'https://www.nexusmods.com/cyberpunk2077/mods/107/files#anchor',
      'https://www.nexusmods.com/cyberpunk2077/mods/107?tab=files&file_id=100',
    ]) {
      expect(identifyModPage(url)?.modId, url).toBe('107');
    }
  });

  it('normalises the game domain to lower case', () => {
    expect(
      identifyModPage('https://www.nexusmods.com/SkyrimSpecialEdition/mods/1')?.gameDomain,
    ).toBe('skyrimspecialedition');
  });

  it('accepts hyphenated game domains', () => {
    expect(identifyModPage('https://www.nexusmods.com/baldursgate3/mods/1')?.gameDomain).toBe(
      'baldursgate3',
    );
    expect(identifyModPage('https://www.nexusmods.com/stardew-valley/mods/1')?.gameDomain).toBe(
      'stardew-valley',
    );
  });

  it('refuses pages that are not mod pages', () => {
    for (const url of [
      'https://www.nexusmods.com/',
      'https://www.nexusmods.com/cyberpunk2077',
      'https://www.nexusmods.com/cyberpunk2077/mods',
      'https://www.nexusmods.com/cyberpunk2077/users/1',
      'https://www.nexusmods.com/cyberpunk2077/mods/notanumber',
    ]) {
      expect(identifyModPage(url), url).toBeNull();
    }
  });

  it('refuses other hosts and other schemes', () => {
    for (const url of [
      'https://evil.example.com/cyberpunk2077/mods/107',
      'https://www.nexusmods.com.evil.example/cyberpunk2077/mods/107',
      'http://www.nexusmods.com/cyberpunk2077/mods/107',
      'file:///cyberpunk2077/mods/107',
      'javascript:alert(1)',
    ]) {
      expect(identifyModPage(url), url).toBeNull();
    }
  });

  it('refuses malformed input rather than throwing', () => {
    for (const url of ['', 'not a url', '///', 'https://']) {
      expect(() => identifyModPage(url)).not.toThrow();
      expect(identifyModPage(url), url).toBeNull();
    }
  });

  it('refuses over-long identifiers', () => {
    const long = 'a'.repeat(200);
    expect(identifyModPage(`https://www.nexusmods.com/${long}/mods/1`)).toBeNull();
    expect(
      identifyModPage(`https://www.nexusmods.com/cyberpunk2077/mods/${'1'.repeat(200)}`),
    ).toBeNull();
  });

  it('cannot be tricked into emitting a traversal', () => {
    // `URL` collapses `..` before the pattern is applied, so a crafted path
    // stops matching and is refused rather than yielding a mangled identifier.
    expect(
      identifyModPage('https://www.nexusmods.com/cyberpunk2077/mods/107/../../admin'),
    ).toBeNull();
    expect(identifyModPage('https://www.nexusmods.com/cyberpunk2077/mods/%2E%2E/admin')).toBeNull();

    // Encoded separators do not survive either.
    expect(identifyModPage('https://www.nexusmods.com/cyberpunk2077%2Fmods%2F107')).toBeNull();
  });

  it('only ever returns a slug and digits', () => {
    const identity = identifyModPage('https://www.nexusmods.com/cyberpunk2077/mods/107?x=../..');
    expect(identity?.gameDomain).toMatch(/^[a-z0-9-]+$/);
    expect(identity?.modId).toMatch(/^\d+$/);
  });

  it('isModPage agrees with identifyModPage', () => {
    expect(isModPage('https://www.nexusmods.com/cyberpunk2077/mods/107')).toBe(true);
    expect(isModPage('https://www.nexusmods.com/')).toBe(false);
  });
});
