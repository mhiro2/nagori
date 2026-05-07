import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { ServerOptions } from 'vite';
import { defineConfig } from 'vite';

const TAURI_DEV_HOST = process.env['TAURI_DEV_HOST'];

// Tauri injects TAURI_ENV_* during `tauri dev`/`tauri build`. The fixed dev
// port matches what tauri.conf.json's `build.devUrl` will point at.
const server: ServerOptions = {
  port: 1420,
  strictPort: true,
  host: TAURI_DEV_HOST ?? false,
  watch: { ignored: ['**/src-tauri/**'] },
};

if (TAURI_DEV_HOST) {
  server.hmr = { protocol: 'ws', host: TAURI_DEV_HOST, port: 1421 };
}

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server,
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    target: 'es2024',
    sourcemap: !!process.env['TAURI_ENV_DEBUG'],
    minify: process.env['TAURI_ENV_DEBUG'] ? false : 'esbuild',
  },
});
