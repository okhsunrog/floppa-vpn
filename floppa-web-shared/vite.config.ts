import { defineConfig } from 'vite-plus'

export default defineConfig({
  run: {
    tasks: {
      typecheck: {
        command: 'vue-tsc --build --force',
        cache: true,
        input: [{ auto: true }, '!node_modules/.tmp/**'],
        output: [{ auto: true }, '!node_modules/.tmp/**'],
      },
      test: {
        command: 'vp test',
        cache: true,
        dependsOn: ['typecheck'],
        output: [],
      },
    },
  },
  test: {
    include: ['src/**/*.test.ts'],
  },
})
