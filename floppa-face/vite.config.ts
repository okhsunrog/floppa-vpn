import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import ui from '@nuxt/ui/vite'
import vueDevTools from 'vite-plugin-vue-devtools'

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    vue(),
    // WARNING: The autoImport config is critical! Nuxt UI's unplugin-auto-import registers
    // "options" from useResizable.js as an auto-importable name. Since "options" is used as a
    // parameter name everywhere in the generated SDK (and in node_modules like @vue/shared,
    // reka-ui), the plugin injects bogus `import { options } from 'useResizable.js'` causing
    // white screens and "can't access lexical declaration before initialization" errors.
    // `ignore` prevents "options" from being auto-imported. `exclude` skips transforming SDK files.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    ui({ autoImport: { ignore: ['options'], exclude: [/floppa-web-shared\/src\/client/, /node_modules/] } }) as any,
    vueDevTools(),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url))
    },
    dedupe: ['vue', 'pinia', 'vue-i18n', 'vue-router'],
  },
  build: {
    rollupOptions: {
      onwarn(warning, warn) {
        if (warning.code === 'SOURCEMAP_ERROR' && warning.message.includes("Can't resolve original location")) return
        warn(warning)
      },
    },
  },
  server: {
    proxy: {
      '/api': {
        target: 'http://localhost:3000',
        changeOrigin: true,
      },
    },
  },
})
