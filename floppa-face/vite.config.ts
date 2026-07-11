import { fileURLToPath, URL } from 'node:url'
import { defineConfig, lazyPlugins } from 'vite-plus'

// https://vite.dev/config/
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
      }),
      vueDevTools(),
    ]
  }),
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
  build: {
    chunkSizeWarningLimit: 1000,
    rolldownOptions: {
      // Tauri-only modules used in shared views via dynamic import — mark external
      // so Rolldown doesn't try to resolve them in the admin panel build
      external: [
        'tauri-plugin-android-fs-api',
        '@tauri-apps/plugin-dialog',
        '@tauri-apps/plugin-fs',
      ],
      onLog(level, log, handler) {
        if (log.code === 'INVALID_ANNOTATION' && log.id?.includes('@vueuse/core/dist/index.js'))
          return
        if (
          log.code === 'SOURCEMAP_ERROR' &&
          log.message.includes("Can't resolve original location")
        )
          return
        if (log.code === 'INEFFECTIVE_DYNAMIC_IMPORT') return
        handler(level, log)
      },
    },
  },
  server: {
    proxy: {
      // Defaults to a local backend on :3000. Set FLOPPA_DEV_API to proxy at a remote
      // backend instead, e.g. FLOPPA_DEV_API=https://floppa.okhsunrog.dev vp dev
      '/api': {
        target: process.env.FLOPPA_DEV_API || 'http://localhost:3000',
        changeOrigin: true,
        secure: true,
      },
    },
  },
})
