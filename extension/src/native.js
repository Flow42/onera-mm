/**
 * The Native Messaging client.
 *
 * Chromium handles the length-prefixed framing; this module owns the envelope:
 * a protocol version, a request id and a structured error. It also owns the
 * timeout, because a native host that never replies would otherwise leave the
 * popup spinning forever.
 *
 * @module native
 */

/** Registered native host name. Must match the host manifest. */
export const HOST_NAME = 'com.onera.host';

/** Protocol version this extension speaks. */
export const PROTOCOL_VERSION = 1;

/** How long to wait for a reply before giving up. */
const TIMEOUT_MS = 30_000;

/** Error codes the host can return, mirrored for exhaustive handling. */
export const ErrorCode = Object.freeze({
  MALFORMED: 'malformed',
  UNSUPPORTED_VERSION: 'unsupported_version',
  NOT_AUTHENTICATED: 'not_authenticated',
  NOT_FOUND: 'not_found',
  SELECTION_REQUIRED: 'selection_required',
  DECISION_REQUIRED: 'decision_required',
  PROVIDER_ERROR: 'provider_error',
  INTERNAL: 'internal',
});

let counter = 0;

/** @returns {string} A request id unique within this service-worker lifetime. */
function nextId() {
  counter += 1;
  return `ext-${Date.now().toString(36)}-${counter}`;
}

/**
 * Send one command to the native host and await its reply.
 *
 * @param {string} type - Command type, e.g. `"download_and_install"`.
 * @param {Record<string, unknown>} [payload] - Command fields.
 * @returns {Promise<{ ok: true, data: unknown } | { ok: false, code: string, message: string }>}
 */
export async function send(type, payload = {}) {
  const request = { v: PROTOCOL_VERSION, id: nextId(), type, ...payload };

  /** @type {unknown} */
  let response;
  try {
    response = await withTimeout(chrome.runtime.sendNativeMessage(HOST_NAME, request), TIMEOUT_MS);
  } catch (error) {
    // The commonest cause by far is that the host manifest was never installed,
    // so the message says what to do about it rather than echoing a raw error.
    return {
      ok: false,
      code: ErrorCode.INTERNAL,
      message: describeTransportFailure(error),
    };
  }

  return interpret(response, request.id);
}

/**
 * Validate and interpret a reply. Everything from the host is untrusted: a
 * different program could have been registered under the host name.
 *
 * @param {unknown} response - The raw reply.
 * @param {string} expectedId - The id that was sent.
 * @returns {{ ok: true, data: unknown } | { ok: false, code: string, message: string }}
 */
export function interpret(response, expectedId) {
  if (response === null || typeof response !== 'object') {
    return { ok: false, code: ErrorCode.MALFORMED, message: 'The Onera host sent an empty reply.' };
  }

  const { v, id, status, data, code, message } = /** @type {Record<string, unknown>} */ (response);

  if (v !== PROTOCOL_VERSION) {
    return {
      ok: false,
      code: ErrorCode.UNSUPPORTED_VERSION,
      message: `This extension speaks protocol v${PROTOCOL_VERSION} but Onera speaks v${String(v)}. Update whichever is older.`,
    };
  }
  if (id !== expectedId) {
    // A mismatched id means replies and requests have gone out of step; acting
    // on the payload would attribute one mod's result to another.
    return {
      ok: false,
      code: ErrorCode.MALFORMED,
      message: 'The Onera host sent a mismatched reply.',
    };
  }

  if (status === 'ok') {
    return { ok: true, data: data ?? null };
  }
  if (status === 'error') {
    return {
      ok: false,
      code: typeof code === 'string' ? code : ErrorCode.INTERNAL,
      message: typeof message === 'string' ? message : 'Onera reported an error.',
    };
  }
  return {
    ok: false,
    code: ErrorCode.MALFORMED,
    message: 'The Onera host sent an unrecognised reply.',
  };
}

/**
 * Turn a transport failure into something a user can act on.
 *
 * @param {unknown} error - The thrown value.
 * @returns {string}
 */
export function describeTransportFailure(error) {
  const text = error instanceof Error ? error.message : String(error);
  if (/not found|no such native|Specified native messaging host not found/i.test(text)) {
    return 'Onera is not installed, or its browser connector is not registered. See the Native Messaging setup guide.';
  }
  if (/timed out/i.test(text)) {
    return 'Onera did not respond. Is the application running?';
  }
  return 'Could not reach Onera.';
}

/**
 * Reject a promise that takes too long.
 *
 * @template T
 * @param {Promise<T>} promise - The promise to bound.
 * @param {number} ms - Timeout in milliseconds.
 * @returns {Promise<T>}
 */
export function withTimeout(promise, ms) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('timed out')), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}
