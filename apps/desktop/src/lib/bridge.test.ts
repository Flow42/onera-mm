/**
 * The bridge is the single narrowing point between untrusted backend output and
 * typed frontend data, so its error handling is tested directly.
 */
import { describe, expect, it, vi } from 'vitest';
import { BridgeError, commands, normaliseError, setBridge } from './bridge';

describe('normaliseError', () => {
  it('passes a BridgeError through unchanged', () => {
    const original = new BridgeError('not_found', 'gone');
    expect(normaliseError(original)).toBe(original);
  });

  it('accepts the structured error the Tauri commands return', () => {
    const error = normaliseError({ code: 'decision_required', message: '2 conflicts' });
    expect(error.code).toBe('decision_required');
    expect(error.message).toBe('2 conflicts');
  });

  it('accepts a bare string', () => {
    expect(normaliseError('something broke').code).toBe('internal');
    expect(normaliseError('something broke').message).toBe('something broke');
  });

  it('never produces an empty message', () => {
    for (const value of [null, undefined, 42, [], {}]) {
      const error = normaliseError(value);
      expect(error.message.length, String(value)).toBeGreaterThan(0);
      expect(error.code).toBe('internal');
    }
  });
});

describe('commands', () => {
  it('forwards arguments and returns typed results', async () => {
    const invoke = vi.fn().mockResolvedValue(true);
    setBridge({ invoke, listen: vi.fn() });

    await expect(commands.isAuthenticated()).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith('is_authenticated', undefined);

    setBridge(null);
  });

  it('turns a rejection into a BridgeError', async () => {
    setBridge({
      invoke: vi.fn().mockRejectedValue({ code: 'not_authenticated', message: 'no key' }),
      listen: vi.fn(),
    });

    await expect(commands.account()).rejects.toBeInstanceOf(BridgeError);
    await expect(commands.account()).rejects.toMatchObject({ code: 'not_authenticated' });

    setBridge(null);
  });

  it('never sends the API key back to the frontend', async () => {
    const invoke = vi.fn().mockResolvedValue({ username: 'TestUser', premium: true });
    setBridge({ invoke, listen: vi.fn() });

    const account = await commands.setApiKey('secret-key-value');
    expect(JSON.stringify(account)).not.toContain('secret-key-value');

    setBridge(null);
  });
});
