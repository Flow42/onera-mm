/**
 * The extension treats the native host's replies as untrusted: something else
 * could be registered under the host name. These tests cover that boundary.
 */
import { describe, expect, it, vi } from 'vitest';
// @ts-expect-error - plain JS module with JSDoc types.
import {
  describeTransportFailure,
  interpret,
  PROTOCOL_VERSION,
  withTimeout,
} from '../../extension/src/native.js';

describe('interpret', () => {
  it('accepts a well-formed success', () => {
    expect(
      interpret({ v: PROTOCOL_VERSION, id: 'a', status: 'ok', data: { queued: true } }, 'a'),
    ).toEqual({
      ok: true,
      data: { queued: true },
    });
  });

  it('accepts a well-formed error and preserves its code', () => {
    const result = interpret(
      {
        v: PROTOCOL_VERSION,
        id: 'a',
        status: 'error',
        code: 'not_authenticated',
        message: 'no key',
      },
      'a',
    );
    expect(result).toEqual({ ok: false, code: 'not_authenticated', message: 'no key' });
  });

  it('rejects a protocol version mismatch with an actionable message', () => {
    const result = interpret({ v: 99, id: 'a', status: 'ok', data: null }, 'a');
    expect(result.ok).toBe(false);
    expect(result.code).toBe('unsupported_version');
    expect(result.message).toMatch(/update/i);
  });

  it('rejects a reply whose id does not match the request', () => {
    // Acting on it would attribute one mod's result to another.
    const result = interpret({ v: PROTOCOL_VERSION, id: 'other', status: 'ok', data: null }, 'a');
    expect(result.ok).toBe(false);
    expect(result.code).toBe('malformed');
  });

  it('rejects replies that are not objects', () => {
    for (const reply of [null, undefined, 'ok', 42, true]) {
      expect(interpret(reply, 'a').ok, String(reply)).toBe(false);
    }
  });

  it('rejects an unrecognised status', () => {
    expect(interpret({ v: PROTOCOL_VERSION, id: 'a', status: 'maybe' }, 'a').ok).toBe(false);
  });

  it('substitutes a safe message when the host omits one', () => {
    const result = interpret({ v: PROTOCOL_VERSION, id: 'a', status: 'error' }, 'a');
    expect(result.code).toBe('internal');
    expect(result.message).toBeTruthy();
  });
});

describe('describeTransportFailure', () => {
  it('explains a missing host manifest rather than echoing the raw error', () => {
    const message = describeTransportFailure(
      new Error('Specified native messaging host not found.'),
    );
    expect(message).toMatch(/not installed|not registered/i);
  });

  it('explains a timeout', () => {
    expect(describeTransportFailure(new Error('timed out'))).toMatch(/did not respond/i);
  });

  it('falls back to a generic message for anything else', () => {
    expect(describeTransportFailure({ weird: true })).toBe('Could not reach Onera.');
  });
});

describe('withTimeout', () => {
  it('resolves when the promise is fast enough', async () => {
    await expect(withTimeout(Promise.resolve('value'), 1000)).resolves.toBe('value');
  });

  it('rejects when the promise never settles', async () => {
    vi.useFakeTimers();
    const pending = withTimeout(new Promise(() => {}), 100);
    const assertion = expect(pending).rejects.toThrow(/timed out/);
    await vi.advanceTimersByTimeAsync(200);
    await assertion;
    vi.useRealTimers();
  });

  it('propagates the original rejection unchanged', async () => {
    await expect(withTimeout(Promise.reject(new Error('boom')), 1000)).rejects.toThrow('boom');
  });
});
