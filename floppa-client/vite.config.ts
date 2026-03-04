import { readFileSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import vueDevTools from 'vite-plugin-vue-devtools'
import ui from '@nuxt/ui/vite'
import JSON5 from 'json5'

const host = process.env.TAURI_DEV_HOST
const isProduction = process.env.NODE_ENV === 'production'

// Read app version from tauri.conf.json5 (single source of truth)
const tauriConf = JSON5.parse(readFileSync('./src-tauri/tauri.conf.json5', 'utf-8'))
const appVersion: string = tauriConf.version ?? '0.0.0'

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(appVersion),
  },

  plugins: [
    vue(),
    // WARNING: The autoImport config is critical! Nuxt UI's unplugin-auto-import registers
    // "options" from useResizable.js as an auto-importable name. Since "options" is used as a
    // parameter name everywhere in the generated SDK (and in node_modules like @vue/shared,
    // reka-ui), the plugin injects bogus `import { options } from 'useResizable.js'` causing
    // white screens and "can't access lexical declaration before initialization" errors.
    // `ignore` prevents "options" from being auto-imported. `exclude` skips transforming SDK files.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any -- ui() plugin type mismatch with Vite's PluginOption
    ui({ autoImport: { ignore: ['options'], exclude: [/floppa-web-shared\/src\/client/, /node_modules/] } }) as any,
    ...(!isProduction ? [vueDevTools()] : []),
  ],

  // 1. prevent vite from obscuring rust errors
  clearScreen: false,

  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || '127.0.0.1',
    hmr: host
      ? {
          protocol: 'ws',
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // tell vite to ignore watching `src-tauri`
      ignored: ['**/src-tauri/**'],
    },
  },

  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
    dedupe: ['vue', 'pinia', 'vue-i18n', 'vue-router'],
  },

  // Pre-bundle all deps upfront so Vite doesn't re-optimize mid-serve
  // (Tauri webview doesn't handle 504 "Outdated Optimize Dep" like a browser)
  optimizeDeps: {
    include: [
      'vue',
      'vue-router',
      'pinia',
      'vue-i18n',
      '@pinia/colada',
      '@hey-api/client-fetch',
      '@tauri-apps/api/webviewWindow',
      '@tauri-apps/plugin-os',
      '@tauri-apps/plugin-shell',
    ],
  },

  build: {
    chunkSizeWarningLimit: 2000, // Tauri app — single bundle, no CDN concerns
    rollupOptions: {
      onwarn(warning, warn) {
        if (warning.code === 'INEFFECTIVE_DYNAMIC_IMPORT') return
        warn(warning)
      },
    },
  },

  // ensure proper resource paths in bundled app
  base: './',
})
