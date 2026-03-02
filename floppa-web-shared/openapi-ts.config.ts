import { defineConfig } from '@hey-api/openapi-ts'

export default defineConfig({
  input: './openapi.json',
  output: 'src/client',
  plugins: [
    '@hey-api/typescript',
    '@hey-api/sdk',
    {
      name: '@hey-api/client-fetch',
      baseUrl: '/api',
    },
    '@pinia/colada',
  ],
})
