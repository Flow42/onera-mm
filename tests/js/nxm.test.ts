/**
 * A `nxm://` link is the only way an account without a paid tier can obtain a
 * download, and it arrives from a page: it is untrusted input carrying a nonce.
 * These tests are the specification of what the extension will accept as one.
 */
import { describe, expect, it } from 'vitest';
// @ts-expect-error - the extension ships plain JS with JSDoc types.
import { findNxmUrl, parseNxmUrl } from '../../extension/src/nxm.js';

const LINK =
  'nxm://cyberpunk2077/mods/4198/files/154093?key=Ab3-_cd9&expires=1757200000&user_id=38236315';

describe('parseNxmUrl', () => {
  it('reads the mod, the file and the permission the site minted', () => {
    expect(parseNxmUrl(LINK)).toEqual({
      gameDomain: 'cyberpunk2077',
      modId: '4198',
      // The site's own id for the file, which is not the id the API keys on.
      fileId: '154093',
      grant: { key: 'Ab3-_cd9', expires: 1757200000 },
    });
  });

  it('accepts a link with no permission on it, as a premium account gets', () => {
    expect(parseNxmUrl('nxm://cyberpunk2077/mods/4198/files/154093')).toMatchObject({
      fileId: '154093',
      grant: null,
    });
  });

  it('normalises the game domain the way a page URL is normalised', () => {
    expect(parseNxmUrl('nxm://CyberPunk2077/mods/4198/files/154093')?.gameDomain).toBe(
      'cyberpunk2077',
    );
  });

  it('refuses anything that is not a download link', () => {
    for (const raw of [
      undefined,
      42,
      '',
      'https://www.nexusmods.com/cyberpunk2077/mods/4198',
      // Another scheme wearing the shape.
      'javascript://cyberpunk2077/mods/4198/files/154093',
      // Not a mod file at all.
      'nxm://cyberpunk2077/collections/abc',
      'nxm://cyberpunk2077/mods/4198',
      // Identifiers that are not identifiers.
      'nxm://cyberpunk2077/mods/../../files/1',
      'nxm://cyber punk/mods/4198/files/154093',
      `nxm://cyberpunk2077/mods/${'9'.repeat(65)}/files/1`,
    ]) {
      expect(parseNxmUrl(raw), String(raw)).toBeNull();
    }
  });

  it('drops a permission it cannot vouch for rather than passing it on', () => {
    for (const query of [
      '?key=has%20a%20space&expires=1757200000',
      '?key=' + 'a'.repeat(129) + '&expires=1757200000',
      '?key=Ab3&expires=not-a-number',
      // Half a grant is no grant: an expiry that cannot be read cannot be
      // checked, and a key with no expiry cannot be judged fresh.
      '?key=Ab3',
      '?expires=1757200000',
    ]) {
      const link = parseNxmUrl(`nxm://cyberpunk2077/mods/4198/files/154093${query}`);
      expect(link, query).not.toBeNull();
      expect(link.grant, query).toBeNull();
    }
  });
});

describe('findNxmUrl', () => {
  it('finds the link in the response that carried it', () => {
    expect(findNxmUrl(`{"url":"${LINK}","name":"Nexus CDN"}`)).toBe(LINK);
  });

  it('survives the slash escaping a JSON body may use', () => {
    const escaped = 'nxm:\\/\\/cyberpunk2077\\/mods\\/4198\\/files\\/154093?key=Ab3&expires=1';
    expect(findNxmUrl(`{"url":"${escaped}"}`)).toBe(
      'nxm://cyberpunk2077/mods/4198/files/154093?key=Ab3&expires=1',
    );
  });

  it('finds nothing in a body that carries no link', () => {
    for (const text of [undefined, '', '{"files":[{"id":154093}]}', 'nxm:// broken']) {
      expect(findNxmUrl(text), String(text)).toBeNull();
    }
  });
});
