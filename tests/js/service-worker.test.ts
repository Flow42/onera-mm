/**
 * The service worker is the only component that can reach the native host, so
 * it is also the only place that can send it something a page chose. These
 * tests cover that boundary: what reaches the host, and what the worker does
 * about the desktop window before queueing work for it.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const sent: { type: string; payload: Record<string, unknown> }[] = [];
/** Replies the fake host gives, keyed by command type. */
let replies: Record<string, unknown> = {};

vi.mock('../../extension/src/native.js', () => ({
  send: vi.fn(async (type: string, payload: Record<string, unknown> = {}) => {
    sent.push({ type, payload });
    return replies[type] ?? { ok: true, data: {} };
  }),
}));

// The worker registers a listener at import time; under Node there is no
// extension runtime to register with, so the one API it touches is stubbed.
vi.stubGlobal('chrome', { runtime: { onMessage: { addListener: vi.fn() } } });

// @ts-expect-error - plain JS module with JSDoc types.
const { handle, ensureDesktop } = await import('../../extension/src/service-worker.js');

const MOD_PAGE = 'https://www.nexusmods.com/cyberpunk2077/mods/107';

/** Every command type the worker sent, in order. */
function types(): string[] {
  return sent.map((entry) => entry.type);
}

/** The payload of the last message of one type. */
function payloadOf(type: string): Record<string, unknown> {
  const match = [...sent].reverse().find((entry) => entry.type === type);
  if (match === undefined) throw new Error(`no ${type} was sent`);
  return match.payload;
}

beforeEach(() => {
  sent.length = 0;
  replies = { app_state: { ok: true, data: { running: true, launchable: true } } };
});

describe('handle', () => {
  it('refuses anything that is not a mod page', async () => {
    for (const url of ['https://example.test/mods/1', 'not a url', undefined]) {
      const result = await handle({ action: 'download', url });
      expect(result.ok).toBe(false);
      expect(result.code).toBe('malformed');
    }
    expect(types()).not.toContain('download');
  });

  it('refuses an unknown action', async () => {
    const result = await handle({ action: 'rm -rf', url: MOD_PAGE });
    expect(result).toMatchObject({ ok: false, code: 'malformed' });
  });

  it('sends only the identity it re-derived, never the page it was given', async () => {
    // A tab URL carrying a query string and a tab: neither should reach Onera.
    await handle({
      action: 'download',
      url: `${MOD_PAGE}?tab=files&session=secret#top`,
    });
    expect(payloadOf('download')).toEqual({
      game_domain: 'cyberpunk2077',
      mod_id: '107',
      file_id: null,
      page_url: MOD_PAGE,
    });
  });

  it('passes a chosen file through and records the canonical page', async () => {
    await handle({ action: 'download_and_install', url: MOD_PAGE, fileId: '100' });
    expect(payloadOf('download_and_install')).toMatchObject({
      file_id: '100',
      page_url: MOD_PAGE,
    });
  });

  it('records no page address for an add, which transfers nothing', async () => {
    await handle({ action: 'add_mod', url: MOD_PAGE });
    expect(payloadOf('add_mod')).toEqual({ game_domain: 'cyberpunk2077', mod_id: '107' });
  });

  it('starts the desktop application before queueing a transfer', async () => {
    replies.app_state = { ok: true, data: { running: false, launchable: true } };
    replies.launch_app = { ok: true, data: { running: false, launched: true } };
    const result = await handle({ action: 'download', url: MOD_PAGE });
    expect(types()).toEqual(['app_state', 'launch_app', 'download']);
    expect(result.desktop).toEqual({ running: false, launched: true });
  });

  it('does not start a window that is already running', async () => {
    await handle({ action: 'download', url: MOD_PAGE });
    expect(types()).toEqual(['app_state', 'download']);
  });

  it('queues the request even when the window could not be started', async () => {
    replies.app_state = { ok: true, data: { running: false, launchable: false } };
    const result = await handle({ action: 'download_and_install', url: MOD_PAGE });
    // The work is durable in Onera's inbox, so a failed launch must not
    // discard it.
    expect(types()).toContain('download_and_install');
    expect(result.ok).toBe(true);
  });

  it('never starts the window for a request that only records metadata', async () => {
    await handle({ action: 'add_mod', url: MOD_PAGE });
    await handle({ action: 'mod_state', url: MOD_PAGE });
    expect(types()).not.toContain('app_state');
  });

  it('turns a mod state into a view the content script can draw', async () => {
    replies.mod_state = {
      ok: true,
      data: { installed: true, installed_version: '1.0', update_available: false },
    };
    const result = await handle({ action: 'mod_view', url: MOD_PAGE });
    expect(result.ok).toBe(true);
    expect(result.data.reachable).toBe(true);
    expect(result.data.view.kind).toBe('installed');
  });

  it('still returns a usable view when the host cannot answer', async () => {
    replies.mod_state = { ok: false, code: 'provider_error', message: 'offline' };
    const result = await handle({ action: 'mod_view', url: MOD_PAGE });
    expect(result.ok).toBe(true);
    expect(result.data.reachable).toBe(false);
    expect(result.data.message).toBe('offline');
    expect(result.data.view.buttons).toHaveLength(3);
  });
});

describe('ensureDesktop', () => {
  it('reports a running window without trying to start one', async () => {
    expect(await ensureDesktop()).toEqual({
      ok: true,
      data: { running: true, launched: false },
    });
    expect(types()).toEqual(['app_state']);
  });

  it('explains that no window is installed rather than failing silently', async () => {
    replies.app_state = { ok: true, data: { running: false, launchable: false } };
    const result = await ensureDesktop();
    expect(result.ok).toBe(false);
    expect(result.code).toBe('not_found');
    expect(types()).not.toContain('launch_app');
  });

  it('passes a host failure through untouched', async () => {
    replies.app_state = { ok: false, code: 'internal', message: 'no host' };
    expect(await ensureDesktop()).toEqual({ ok: false, code: 'internal', message: 'no host' });
  });
});
