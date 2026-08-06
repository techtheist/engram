import { fileURLToPath, URL } from 'node:url'

import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import vueDevTools from 'vite-plugin-vue-devtools'

/**
 * ENGRAM_DEMO=1 builds the static GitHub Pages demo instead of the pane the
 * daemon serves: the backend module is swapped for the in-browser mock under
 * `demo/`, and the demo's banner is mounted through the host-chrome slot.
 *
 * It is a module swap, not a runtime flag, so neither build carries the
 * other's code — the daemon never ships mock data, and the demo never ships a
 * fetch client aimed at a daemon that isn't there. Output goes to
 * `dist-demo/`, deliberately NOT `dist/`: that directory is what rust-embed
 * bakes into the release binary, so a demo build landing there would ship
 * inside the next `scripts/deploy-pane.sh`.
 */
const demo = process.env.ENGRAM_DEMO === '1'

const path = (p: string): string => fileURLToPath(new URL(p, import.meta.url))

// https://vite.dev/config/
export default defineConfig({
  base: './',
  plugins: [
    vue(),
    tailwindcss(),
    // Devtools inject a websocket client; the demo has nothing to talk to.
    ...(demo ? [] : [vueDevTools()]),
  ],
  build: demo ? { outDir: 'dist-demo', emptyOutDir: true } : {},
  resolve: {
    alias: {
      // Specific entries first — the bare '@' prefix would swallow them.
      ...(demo
        ? {
            '@/services/api': path('./demo/api.ts'),
            '@/services/hostChrome': path('./demo/chrome.ts'),
          }
        : {}),
      '@': path('./src'),
    },
  },
})
