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
        string: (ctx) => {
          if (ctx.schema.format === 'date-time') {
            return ctx
              .$(ctx.plugin.imports.z)
              .attr('iso')
              .attr('datetime')
              .call(ctx.$.object().prop('offset', ctx.$.literal(true)))
          }
        },
        number: (ctx) => {
          // The API and TypeScript SDK use JSON numbers. Zod's default int64
          // coercion would accept strings and booleans as IDs; validate without coercion.
          if (ctx.schema.format === 'int64' || ctx.schema.format === 'uint64') {
            let schema = ctx.$(ctx.plugin.imports.z).attr('int').call()
            if (ctx.schema.format === 'uint64') schema = schema.attr('nonnegative').call()
            if (ctx.schema.minimum !== undefined)
              schema = schema.attr('min').call(ctx.schema.minimum)
            if (ctx.schema.maximum !== undefined)
              schema = schema.attr('max').call(ctx.schema.maximum)
            if (ctx.schema.exclusiveMinimum !== undefined)
              schema = schema.attr('gt').call(ctx.schema.exclusiveMinimum)
            if (ctx.schema.exclusiveMaximum !== undefined)
              schema = schema.attr('lt').call(ctx.schema.exclusiveMaximum)
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
