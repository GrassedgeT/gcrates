import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

const crateRoot = fileURLToPath(new URL('..', import.meta.url))
const wasmEntry = fileURLToPath(new URL('../pkg/gcrates.js', import.meta.url))
const mainEntry = fileURLToPath(new URL('./index.html', import.meta.url))

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],
  resolve: {
    alias: {
      gcrates: wasmEntry,
    },
  },
  server: {
    fs: {
      allow: [crateRoot],
    },
  },
  build: {
    rollupOptions: {
      input: {
        index: mainEntry,
      },
    },
  },
})
