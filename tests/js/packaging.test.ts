/**
 * The extension ships as a list of files rather than a directory, because the
 * Tauri bundler's `files` map takes one entry per file. That is fine until
 * someone adds a module and forgets the entry — at which point the packaged
 * extension is silently broken in a way no unit test would notice, and only a
 * user loading it from `/usr/share/onera/extension` would ever see.
 *
 * These tests are the thing that notices.
 */
import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../..', import.meta.url));

const tauriConfig = JSON.parse(
  readFileSync(join(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'),
);
const extensionManifest = JSON.parse(readFileSync(join(root, 'extension/manifest.json'), 'utf8'));

/** Every file under `extension/`, as a repo-relative path. */
function extensionFiles(directory = 'extension'): string[] {
  return readdirSync(join(root, directory), { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory()
      ? extensionFiles(`${directory}/${entry.name}`)
      : [`${directory}/${entry.name}`],
  );
}

/** Sources of the bundler's `files` map, normalised to repo-relative paths. */
function packagedSources(): string[] {
  const files: Record<string, string> = tauriConfig.bundle.linux.deb.files;
  return Object.values(files).map((source) => source.replace(/^(\.\.\/)+/u, ''));
}

describe('the packaged extension', () => {
  it('ships every file the extension directory contains', () => {
    const packaged = new Set(packagedSources());
    const missing = extensionFiles().filter((file) => !packaged.has(file));
    expect(
      missing,
      'add these to bundle.linux.deb.files in tauri.conf.json, or the packaged ' +
        'extension will be missing them',
    ).toEqual([]);
  });

  it('ships nothing that is not in the extension directory', () => {
    const present = new Set(extensionFiles());
    const stale = packagedSources().filter(
      (source) => source.startsWith('extension/') && !present.has(source),
    );
    expect(stale, 'these packaged paths no longer exist').toEqual([]);
  });

  it('installs every extension file under one directory the user can load', () => {
    const files: Record<string, string> = tauriConfig.bundle.linux.deb.files;
    for (const [target, source] of Object.entries(files)) {
      if (!source.includes('extension/')) continue;
      expect(target).toMatch(/^\/usr\/share\/onera\/extension\//u);
      // The layout has to survive the copy: `manifest.json` refers to
      // `src/content.js`, so a flattened install would not load.
      expect(target.replace('/usr/share/onera/extension/', '')).toBe(
        source.replace(/^(\.\.\/)+extension\//u, ''),
      );
    }
  });
});

describe('versions', () => {
  it('match between the extension and the application', () => {
    // The Native Messaging protocol assumes the two ship together: a mismatch
    // is what `unsupported_version` exists to report, and shipping a package
    // whose halves disagree would produce it on a fresh install.
    expect(extensionManifest.version).toBe(tauriConfig.version);
  });

  it('are a plain three-part version, which is all Chromium accepts', () => {
    expect(extensionManifest.version).toMatch(/^\d+\.\d+\.\d+$/u);
  });
});

describe('the extension manifest', () => {
  it('pins the key that fixes the extension id', () => {
    // Without it an unpacked load gets a random id, and the host's
    // `allowed_origins` — which names one id — refuses every message.
    expect(typeof extensionManifest.key).toBe('string');
    expect(extensionManifest.key.length).toBeGreaterThan(100);
  });

  it('names an id the shipped host manifest allows', () => {
    const host = JSON.parse(readFileSync(join(root, 'packaging/com.onera.host.json'), 'utf8'));
    // The id itself is derived from the key by the browser and cannot be
    // recomputed here, so this pins the pair that were verified together.
    expect(host.allowed_origins).toEqual(['chrome-extension://pohiidkpoflhifciokepgpaandghjgmj/']);
  });
});
