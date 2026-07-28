import js from '@eslint/js';
import globals from 'globals';
import tseslint from 'typescript-eslint';
import svelte from 'eslint-plugin-svelte';
import prettier from 'eslint-config-prettier';

/**
 * Lint configuration.
 *
 * The rules that are escalated to errors are the ones that have historically
 * caused real bugs in this codebase's shape of code: floating promises around
 * the Tauri bridge, and implicit `any` at the boundary where untrusted data
 * arrives from the extension or the backend.
 */
export default tseslint.config(
  {
    ignores: [
      '**/build/**',
      '**/dist/**',
      '**/.svelte-kit/**',
      '**/target/**',
      '**/node_modules/**',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...svelte.configs['flat/recommended'],
  prettier,
  {
    languageOptions: {
      globals: { ...globals.browser, ...globals.node },
    },
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_' }],
      'no-console': ['warn', { allow: ['warn', 'error'] }],
      eqeqeq: ['error', 'always'],
    },
  },
  {
    // Svelte files hold TypeScript in their `<script lang="ts">` blocks, which
    // the Svelte parser only understands when handed the TypeScript parser.
    files: ['**/*.svelte', '**/*.svelte.ts'],
    languageOptions: {
      parserOptions: { parser: tseslint.parser },
    },
    rules: {
      // Every route in this application is a literal, checked by `svelte-check`
      // against the generated route types. `resolve()` would add indirection
      // without adding safety here.
      'svelte/no-navigation-without-resolve': 'off',
    },
  },
  {
    files: ['extension/**/*.js'],
    languageOptions: {
      globals: { ...globals.browser, chrome: 'readonly' },
    },
  },
);
