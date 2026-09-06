/**
 * Whether a list is drawn as full-width rows or as square tiles.
 *
 * The choice is per list — the games list and the mod list are different
 * questions — and it is remembered, because re-choosing it on every visit is
 * exactly the kind of thing that makes a view feel like a form. Storage is
 * best-effort: a webview with storage denied gets the default and keeps
 * working, so no read or write is allowed to throw into a render.
 */

/** The two shapes a list of cards takes. */
export type ViewMode = 'list' | 'tiles';

const PREFIX = 'onera.view-mode.';

/** Narrow an untrusted stored value. */
function parse(value: string | null, fallback: ViewMode): ViewMode {
  return value === 'list' || value === 'tiles' ? value : fallback;
}

/**
 * Read the remembered mode for one list.
 *
 * @param view - The list's key, unique within the application.
 * @param fallback - The mode to use when nothing is remembered.
 * @returns The mode to start in.
 */
export function readMode(view: string, fallback: ViewMode = 'list'): ViewMode {
  try {
    return parse(globalThis.localStorage?.getItem(PREFIX + view) ?? null, fallback);
  } catch {
    return fallback;
  }
}

/**
 * Remember the mode for one list.
 *
 * @param view - The list's key.
 * @param mode - The mode the user chose.
 */
export function writeMode(view: string, mode: ViewMode): void {
  try {
    globalThis.localStorage?.setItem(PREFIX + view, mode);
  } catch {
    // A remembered preference is a convenience; losing it is not a failure.
  }
}

/** A list's mode, as reactive state a view can bind a toggle to. */
export interface ViewModeState {
  readonly current: ViewMode;
  set(mode: ViewMode): void;
}

/**
 * Create the remembered mode for one list.
 *
 * @param view - The list's key.
 * @param fallback - The mode to use when nothing is remembered.
 * @returns Reactive state, persisted on every change.
 */
export function viewMode(view: string, fallback: ViewMode = 'list'): ViewModeState {
  let mode = $state(readMode(view, fallback));
  return {
    get current() {
      return mode;
    },
    set(next: ViewMode) {
      mode = next;
      writeMode(view, next);
    },
  };
}
