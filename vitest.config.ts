import { defineConfig } from 'vitest/config';

/**
 * Unit tests for the browser extension and the desktop frontend.
 *
 * Two projects because two environments are needed: the extension's URL parsing
 * and native-messaging envelope are plain modules that run under Node, while the
 * frontend's view-models are compiled through the Svelte toolchain. Splitting by
 * directory means no test file has to declare its own environment.
 */
export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: 'extension',
          environment: 'node',
          include: ['tests/js/**/*.test.ts'],
        },
      },
      {
        test: {
          name: 'desktop',
          environment: 'jsdom',
          include: ['apps/desktop/src/**/*.test.ts'],
        },
      },
    ],
    coverage: {
      reporter: ['text', 'lcov'],
      include: ['extension/src/**', 'apps/desktop/src/lib/**'],
    },
  },
});
