/**
 * Which buttons a mod page carries is the extension's only real decision, and
 * the one a user notices immediately when it is wrong: offering "download" for
 * a mod that is already installed, or hiding the update for one that is out of
 * date. These tests pin every case, including the one where Onera could not be
 * asked at all.
 */
import { describe, expect, it } from 'vitest';
// @ts-expect-error - plain JS module with JSDoc types.
import { ACTION, actionsFor } from '../../extension/src/mod-actions.js';

/** Labels of the offered buttons, in order. */
function labels(state: unknown): string[] {
  return actionsFor(state).buttons.map((button: { label: string }) => button.label);
}

/** The action of the single primary button, if there is one. */
function primaryAction(state: unknown): string | null {
  const primary = actionsFor(state).buttons.find((button: { primary: boolean }) => button.primary);
  return primary?.action ?? null;
}

describe('actionsFor', () => {
  it('offers every action for a mod Onera has never seen', () => {
    const view = actionsFor({ game_registered: true, downloaded: false, installed: false });
    expect(view.kind).toBe('none');
    expect(view.buttons.map((b: { action: string }) => b.action)).toEqual([
      ACTION.DOWNLOAD_AND_INSTALL,
      ACTION.DOWNLOAD,
      ACTION.ADD,
    ]);
    expect(primaryAction({ game_registered: true })).toBe(ACTION.DOWNLOAD_AND_INSTALL);
  });

  it('says so when no registered game could take the mod', () => {
    const view = actionsFor({ game_registered: false });
    expect(view.status).toContain('no matching game registered');
    // Every action is still offered: the desktop asks which game, it does not
    // refuse the request.
    expect(view.buttons).toHaveLength(3);
    expect(primaryAction({ game_registered: false })).toBeNull();
  });

  it('reports an already-downloaded mod instead of offering to download it again', () => {
    const view = actionsFor({ game_registered: true, downloaded: true, installed: false });
    expect(view.kind).toBe('downloaded');
    expect(view.status).toContain('Already downloaded');
    expect(labels({ game_registered: true, downloaded: true })).toEqual(['Install']);
  });

  it('reports an installed mod with its version and offers no download', () => {
    const state = {
      game_registered: true,
      downloaded: true,
      installed: true,
      installed_version: '1.4.1',
      update_available: false,
    };
    const view = actionsFor(state);
    expect(view.kind).toBe('installed');
    expect(view.status).toBe('Already installed: 1.4.1');
    expect(labels(state)).toEqual(['Reinstall']);
    // Reinstalling is available but never the obvious thing to do.
    expect(primaryAction(state)).toBeNull();
  });

  it('prompts for the update when a newer version exists', () => {
    const state = {
      game_registered: true,
      installed: true,
      installed_version: '1.4.1',
      update_available: true,
      latest_version: '2.0',
    };
    const view = actionsFor(state);
    expect(view.kind).toBe('update');
    expect(view.status).toBe('Installed: 1.4.1 — update to 2.0 available');
    expect(labels(state)).toEqual(['Update to 2.0', 'Download only']);
    expect(primaryAction(state)).toBe(ACTION.DOWNLOAD_AND_INSTALL);
  });

  it('downloads rather than installs an update when no game is registered', () => {
    const state = {
      game_registered: false,
      installed: true,
      update_available: true,
      latest_version: '2.0',
    };
    expect(primaryAction(state)).toBe(ACTION.DOWNLOAD);
  });

  it('falls back to every action when the host could not be asked', () => {
    for (const state of [null, undefined, {}]) {
      const view = actionsFor(state);
      expect(view.kind).toBe('none');
      expect(view.buttons).toHaveLength(3);
    }
  });

  it('never parses a version string, only displays it', () => {
    const state = {
      game_registered: true,
      installed: true,
      installed_version: 'v2 (hotfix)',
      update_available: true,
      latest_version: 'FINAL-3',
    };
    expect(actionsFor(state).status).toBe('Installed: v2 (hotfix) — update to FINAL-3 available');
  });

  it('describes a version the provider never named', () => {
    const state = { game_registered: true, installed: true, update_available: false };
    expect(actionsFor(state).status).toBe('Already installed: an unnamed version');
  });
});
