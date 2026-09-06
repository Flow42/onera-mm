import { describe, expect, it } from 'vitest';
import {
  depthOf,
  directionBetween,
  gameLabel,
  gameOf,
  gameSections,
  globalSections,
  Level,
} from './navigation';

describe('depthOf', () => {
  it('places the games list above one game, and one game above its sections', () => {
    expect(depthOf('/games')).toBe(Level.Games);
    expect(depthOf('/games/9a1c')).toBe(Level.Game);
    expect(depthOf('/mods')).toBe(Level.Section);
  });

  it('keeps startup and onboarding outside the hierarchy', () => {
    expect(depthOf('/')).toBe(Level.Outside);
    expect(depthOf('/onboarding')).toBe(Level.Outside);
  });

  it('reads a trailing slash as the same address', () => {
    expect(depthOf('/games/')).toBe(Level.Games);
  });
});

describe('directionBetween', () => {
  it('travels forward when the move goes deeper', () => {
    expect(directionBetween('/games', '/games/9a1c')).toBe('forward');
    expect(directionBetween('/games/9a1c', '/mods')).toBe('forward');
  });

  it('travels back when the move goes up', () => {
    expect(directionBetween('/mods', '/games/9a1c')).toBe('back');
    expect(directionBetween('/games/9a1c', '/games')).toBe('back');
  });

  it('is neither for a move within one level', () => {
    expect(directionBetween('/mods', '/updates')).toBe('none');
  });
});

describe('gameOf', () => {
  it('reads the game from a submenu address', () => {
    expect(gameOf('/games/9a1c', '')).toBe('9a1c');
  });

  it('reads the game a section was opened for', () => {
    expect(gameOf('/mods', '?game=9a1c')).toBe('9a1c');
  });

  it('decodes an identifier that had to be escaped', () => {
    expect(gameOf('/games/a%2Fb', '')).toBe('a/b');
  });

  it('reports no game rather than an empty one', () => {
    expect(gameOf('/games', '')).toBeNull();
    expect(gameOf('/downloads', '')).toBeNull();
    expect(gameOf('/mods', '?game=')).toBeNull();
  });
});

describe('gameSections', () => {
  it('carries the game into every section it offers', () => {
    const sections = gameSections('9a1c');
    expect(sections.length).toBeGreaterThan(0);
    for (const section of sections) {
      expect(section.href).toContain('game=9a1c');
      expect(depthOf(section.match)).toBe(Level.Section);
    }
  });

  it('escapes an identifier that is not URL-safe', () => {
    expect(gameSections('a/b')[0]?.href).toContain('game=a%2Fb');
  });

  it('leaves the shared download queue out of one game`s submenu', () => {
    // The queue is global, and a job names the provider's slug rather than a
    // registered installation, so scoping it to a game would be a guess.
    expect(gameSections('9a1c').map((section) => section.key)).not.toContain('downloads');
    expect(globalSections().map((section) => section.key)).toContain('downloads');
  });
});

describe('gameLabel', () => {
  it('names an installation by its own directory', () => {
    expect(gameLabel('/games/SteamLibrary/steamapps/common/Cyberpunk 2077')).toBe('Cyberpunk 2077');
    expect(gameLabel('C:\\Games\\Skyrim Special Edition')).toBe('Skyrim Special Edition');
  });

  it('tolerates a trailing separator rather than showing nothing', () => {
    expect(gameLabel('/games/Cyberpunk 2077/')).toBe('Cyberpunk 2077');
  });
});
