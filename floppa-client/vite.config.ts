import { readFileSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig, lazyPlugins } from 'vite-plus'
import JSON5 from 'json5'

const host = process.env.TAURI_DEV_HOST
const isProduction = process.env.NODE_ENV === 'production'

// Read app version from tauri.conf.json5 (single source of truth)
const tauriConf = JSON5.parse(readFileSync('./src-tauri/tauri.conf.json5', 'utf-8'))
const appVersion: string = tauriConf.version ?? '0.0.0'

export default defineConfig({
  run: {
    tasks: {
      typecheck: {
        command: 'vue-tsc --build --force',
        cache: true,
        input: [{ auto: true }, '!node_modules/.tmp/**'],
        output: [{ auto: true }, '!node_modules/.tmp/**'],
      },
      build: {
        command: 'vp build',
        cache: true,
        dependsOn: ['typecheck', { task: 'typecheck', from: ['dependencies', 'devDependencies'] }],
        input: [
          { auto: true },
          '!node_modules/.nuxt-ui/**',
          '!auto-imports.d.ts',
          '!components.d.ts',
        ],
        output: [
          { auto: true },
          '!node_modules/.nuxt-ui/**',
          '!auto-imports.d.ts',
          '!components.d.ts',
        ],
      },
    },
  },
  define: {
    __APP_VERSION__: JSON.stringify(appVersion),
  },

  plugins: lazyPlugins(async () => {
    const [{ default: vue }, { default: ui }, { default: vueDevTools }] = await Promise.all([
      import('@vitejs/plugin-vue'),
      import('@nuxt/ui/vite'),
      import('vite-plugin-vue-devtools'),
    ])
    return [
      vue(),
      // WARNING: The autoImport config is critical! Nuxt UI's unplugin-auto-import registers
      // "options" from useResizable.js as an auto-importable name. Since "options" is used as a
      // parameter name everywhere in the generated SDK (and in node_modules like @vue/shared,
      // reka-ui), the plugin injects bogus `import { options } from 'useResizable.js'` causing
      // white screens and "can't access lexical declaration before initialization" errors.
      // `ignore` prevents "options" from being auto-imported. `exclude` skips transforming SDK files.
      ui({
        autoImport: {
          ignore: ['options'],
          exclude: [/floppa-web-shared\/src\/client/, /node_modules/],
        },
        ui: {
          // Ensure all modals/slideovers render above the sticky navbar (z-40),
          // and dropdowns render above modals (z-[51])
          modal: { slots: { overlay: 'z-50', content: 'z-50' } },
          slideover: { slots: { overlay: 'z-50', content: 'z-50' } },
          select: { slots: { content: 'z-[51]' } },
          selectMenu: { slots: { content: 'z-[51]' } },
        },
      }),
      ...(!isProduction ? [vueDevTools()] : []),
    ]
  }),

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
      // @nuxt/icon (pulled in by @nuxt/ui) imports this Nuxt-only virtual; provide a
      // no-op stub so it resolves outside Nuxt. See the stub file for the full why.
      '#build/nuxt-icon-client-bundle': fileURLToPath(
        new URL('./src/nuxt-icon-client-bundle.ts', import.meta.url),
      ),
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
      '@tauri-apps/api/webviewWindow',
      '@tauri-apps/plugin-os',
      '@tauri-apps/plugin-shell',
    ],
  },

  build: {
    chunkSizeWarningLimit: 2000, // Tauri app — single bundle, no CDN concerns
    rolldownOptions: {
      onLog(level, log, handler) {
        if (log.code === 'INVALID_ANNOTATION' && log.id?.includes('@vueuse/core/dist/index.js'))
          return
        if (log.code === 'INEFFECTIVE_DYNAMIC_IMPORT') return
        handler(level, log)
      },
    },
  },

  // ensure proper resource paths in bundled app
  base: './',
})
