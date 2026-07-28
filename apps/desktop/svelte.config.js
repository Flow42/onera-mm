import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/**
 * Tauri serves the frontend from disk, so the app is prerendered to static
 * files. There is no server: every fetch goes through the Tauri command bridge,
 * which is the only channel to the Rust core.
 *
 * @type {import('@sveltejs/kit').Config}
 */
export default {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({ fallback: 'index.html', strict: false }),
    alias: { $lib: './src/lib' },
  },
};
