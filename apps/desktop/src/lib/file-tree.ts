/**
 * Turning a mod's flat file list into something readable.
 *
 * The backend answers "which files does this mod own" with paths, because that
 * is what it recorded when it deployed them. A list of forty paths that share
 * three directories tells the user very little, so the paths are folded back
 * into the shape they have on disk — and the fold is done here, as a pure
 * function, so what the view claims about a mod can be tested without a DOM.
 *
 * Nothing is invented: every leaf is a file the backend named, and every branch
 * is a directory that appears in one of those paths.
 */

/** One file, as the backend reports it. */
export interface ModFileEntry {
  root_key: string;
  path: string;
  absolute: string;
  exists: boolean;
  size: number | null;
}

/** A node in the folded tree. */
export interface TreeNode {
  /** Stable key within its parent, used for list identity. */
  key: string;
  /** The segment shown: a directory or a file name. */
  name: string;
  /** How deep it sits, so a flat list can be indented without recursion. */
  depth: number;
  /** Whether this node is a file rather than a directory. */
  file: boolean;
  /** Whether the file is still on disk. Directories are never marked missing. */
  missing: boolean;
  /** Size in bytes, for a file that is still there. */
  size: number | null;
}

/**
 * Fold entries into a flat, indented list of directories and files.
 *
 * Flat rather than nested on purpose: the view draws rows, and a nested
 * structure would only be walked back into rows by the template. Deployment
 * roots become the outermost level, because a mod that writes into two roots is
 * writing into two different places and the view must not merge them.
 *
 * @param entries - Files the mod owns.
 * @returns Rows in display order.
 */
export function foldIntoTree(entries: ModFileEntry[]): TreeNode[] {
  const rows: TreeNode[] = [];
  const opened = new Set<string>();

  for (const entry of entries) {
    const segments = entry.path.split('/').filter((segment) => segment.length > 0);
    const branches = segments.slice(0, -1);
    const leaf = segments[segments.length - 1] ?? entry.path;

    // The root is a level of its own, so two roots never look like one tree.
    let prefix = entry.root_key;
    if (!opened.has(prefix)) {
      opened.add(prefix);
      rows.push({
        key: prefix,
        name: entry.root_key,
        depth: 0,
        file: false,
        missing: false,
        size: null,
      });
    }

    let depth = 1;
    for (const branch of branches) {
      prefix = `${prefix}/${branch}`;
      if (!opened.has(prefix)) {
        opened.add(prefix);
        rows.push({ key: prefix, name: branch, depth, file: false, missing: false, size: null });
      }
      depth += 1;
    }

    rows.push({
      key: `${prefix}/${leaf}`,
      name: leaf,
      depth,
      file: true,
      missing: !entry.exists,
      size: entry.size,
    });
  }

  return rows;
}

/**
 * A size in bytes, rendered for a file list.
 *
 * Binary units, because these are files on a disk and every other size in Onera
 * is reported the same way. A file the backend could not stat has no size, and
 * gets none here rather than a zero it did not measure.
 *
 * @param bytes - The size, or null.
 * @returns A short string, or null when there was nothing to format.
 */
export function formatSize(bytes: number | null): string | null {
  if (bytes === null || !Number.isFinite(bytes) || bytes < 0) {
    return null;
  }
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // Whole bytes read oddly with a decimal; everything else is clearer with one.
  const rendered = unit === 0 ? String(Math.round(value)) : value.toFixed(value < 10 ? 1 : 0);
  return `${rendered} ${units[unit]}`;
}

/**
 * The sentence under a truncated file list.
 *
 * @param shown - How many files the view is drawing.
 * @param total - How many the mod owns.
 * @returns A line to show, or null when nothing was left out.
 */
export function remainderNote(shown: number, total: number): string | null {
  const hidden = total - shown;
  if (hidden <= 0) {
    return null;
  }
  return hidden === 1 ? '1 more file' : `${hidden} more files`;
}
