import { beforeEach, describe, expect, it, vi } from 'vitest';
import { readMode, writeMode } from './view-mode.svelte';

describe('view mode', () => {
  beforeEach(() => {
    localStorage.clear();
    vi.restoreAllMocks();
  });

  it('remembers a list`s shape separately from another list`s', () => {
    writeMode('games', 'tiles');
    expect(readMode('games')).toBe('tiles');
    expect(readMode('mods')).toBe('list');
  });

  it('takes the caller`s default when nothing is remembered', () => {
    expect(readMode('games', 'tiles')).toBe('tiles');
  });

  it('ignores a stored value that is not one of the two shapes', () => {
    localStorage.setItem('onera.view-mode.games', 'carousel');
    expect(readMode('games')).toBe('list');
  });

  it('keeps working when storage refuses to answer', () => {
    // A webview with site data blocked throws on access; a remembered layout is
    // a convenience and must never take a view down with it.
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('denied');
    });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('denied');
    });
    expect(readMode('games', 'tiles')).toBe('tiles');
    expect(() => writeMode('games', 'list')).not.toThrow();
  });
});
