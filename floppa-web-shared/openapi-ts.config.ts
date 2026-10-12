import { defineConfig } from '@hey-api/openapi-ts'

export default defineConfig({
  input: './openapi.json',
  output: 'src/client',
  plugins: [
    '@hey-api/typescript',
    { name: '@hey-api/sdk', validator: { request: false, response: 'zod' } },
    {
      name: 'zod',
      $resolvers: {
        number: (ctx) => {
          // The API and TypeScript SDK use JSON numbers. Zod's default int64
          // coercion would accept strings and booleans as IDs; validate without coercion.
          if (ctx.schema.format === 'int64' || ctx.schema.format === 'uint64') {
            let schema = ctx.$(ctx.plugin.imports.z).attr('int').call()
            if (ctx.schema.format === 'uint64') schema = schema.attr('nonnegative').call()
            return schema
          }
        },
      },
    },
    {
      name: '@hey-api/client-fetch',
      baseUrl: '/api',
    },
    '@pinia/colada',
  ],
})
