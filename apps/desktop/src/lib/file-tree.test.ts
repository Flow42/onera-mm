/**
 * What the expanded file view claims about a mod.
 *
 * The fold is the whole view-model: everything the user sees about which files
 * a mod owns comes out of these three functions, so they are tested rather than
 * the markup that draws them.
 */
import { describe, expect, it } from 'vitest';
import { foldIntoTree, formatSize, remainderNote, type ModFileEntry } from './file-tree';

function file(path: string, overrides: Partial<ModFileEntry> = {}): ModFileEntry {
  return {
    root_key: 'game',
    path,
    absolute: `/games/cp/${path}`,
    exists: true,
    size: 1024,
    ...overrides,
  };
}

describe('foldIntoTree', () => {
  it('folds shared directories into one branch each', () => {
    const rows = foldIntoTree([file('archive/pc/mod/a.archive'), file('archive/pc/mod/b.archive')]);
    expect(rows.map((row) => row.name)).toEqual([
      'game',
      'archive',
      'pc',
      'mod',
      'a.archive',
      'b.archive',
    ]);
    // Each level is one step further in, so the view can indent without
    // walking a nested structure.
    expect(rows.map((row) => row.depth)).toEqual([0, 1, 2, 3, 4, 4]);
    expect(rows.filter((row) => row.file)).toHaveLength(2);
  });

  it('keeps two deployment roots apart', () => {
    // A mod that writes into the game and into a user-data directory has
    // written into two different places; merging them would misreport it.
    const rows = foldIntoTree([
      file('r6/scripts/a.reds'),
      file('config.json', { root_key: 'userdata' }),
    ]);
    const roots = rows.filter((row) => row.depth === 0).map((row) => row.name);
    expect(roots).toEqual(['game', 'userdata']);
  });

  it('marks a file that is no longer on disk', () => {
    const [, leaf] = foldIntoTree([file('a.archive', { exists: false, size: null })]);
    expect(leaf).toMatchObject({ name: 'a.archive', file: true, missing: true, size: null });
  });

  it('gives every row a key unique to its place in the tree', () => {
    // Two files with the same name under different directories must not
    // collide, or the list would drop one of them.
    const rows = foldIntoTree([file('pc/mod/a.archive'), file('pc/other/a.archive')]);
    const keys = rows.map((row) => row.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it('has nothing to draw for a mod with no files', () => {
    expect(foldIntoTree([])).toEqual([]);
  });
});

describe('formatSize', () => {
  it('reports bytes as bytes and larger sizes in binary units', () => {
    expect(formatSize(0)).toBe('0 B');
    expect(formatSize(900)).toBe('900 B');
    expect(formatSize(1024)).toBe('1.0 KiB');
    expect(formatSize(1024 * 1024 * 3.5)).toBe('3.5 MiB');
  });

  it('says nothing about a size it was not given', () => {
    expect(formatSize(null)).toBeNull();
    expect(formatSize(Number.NaN)).toBeNull();
  });
});

describe('remainderNote', () => {
  it('counts what the cap left out', () => {
    expect(remainderNote(10, 24)).toBe('14 more files');
    expect(remainderNote(10, 11)).toBe('1 more file');
  });

  it('says nothing when the list is complete', () => {
    expect(remainderNote(4, 4)).toBeNull();
    expect(remainderNote(10, 3)).toBeNull();
  });
});
