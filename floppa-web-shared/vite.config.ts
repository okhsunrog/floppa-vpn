import { defineConfig } from 'vite-plus'

export default defineConfig({
  run: {
    tasks: {
      typecheck: {
        command: 'vue-tsc --build --force',
        cache: {
          input: [{ auto: true }, '!node_modules/.tmp/**'],
          output: [{ auto: true }, '!node_modules/.tmp/**'],
        },
      },
      test: {
        command: 'vp test',
        cache: {
          output: [],
        },
        dependsOn: ['typecheck'],
      },
    },
  },
  test: {
    include: ['src/**/*.test.ts', 'vite/**/*.test.ts'],
  },
})
