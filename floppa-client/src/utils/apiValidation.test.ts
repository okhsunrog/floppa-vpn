import { installApiInterceptors } from '../../../floppa-web-shared/src/api/interceptors'
import { zLinkStartResponse } from 'floppa-web-shared/client/zod.gen'
import { describe, expect, it } from 'vite-plus/test'
import { createClient } from 'floppa-web-shared/client/client'
import { exchangeTelegramLoginCode, deleteMySession } from 'floppa-web-shared/client/sdk.gen'
function api(body: unknown) {
  return createClient({
    baseUrl: 'https://example.test/api',
    fetch: async () => Response.json(body),
  })
}
const valid = { token: 'token', user: { id: 42, is_admin: false, first_name: null } }
describe('generated response validation', () => {
  it('accepts nullable fields and extra fields without changing ID types', async () => {
    const result = await exchangeTelegramLoginCode({
      client: api({ ...valid, future: true }),
      body: { code: 'code' },
      throwOnError: true,
    })
    expect(result.data?.user.id).toBe(42)
    expect(result.data?.user.first_name).toBeNull()
  })
  it.each([
    {},
    { ...valid, token: 123 },
    { ...valid, user: { ...valid.user, id: '42' } },
    { ...valid, user: { ...valid.user, id: true } },
  ])('rejects incompatible login responses', async (body) => {
    await expect(
      exchangeTelegramLoginCode({ client: api(body), body: { code: 'code' }, throwOnError: true }),
    ).rejects.toBeDefined()
  })
})

it('accepts RFC3339 offsets permitted by OpenAPI date-time', () => {
  expect(
    zLinkStartResponse.safeParse({
      code: 'code',
      deep_link: 'tg://test',
      expires_at: '2026-10-12T15:00:00+03:00',
    }).success,
  ).toBe(true)
  expect(
    zLinkStartResponse.safeParse({ code: 'code', deep_link: 'tg://test', expires_at: 'yesterday' })
      .success,
  ).toBe(false)
})

it('rejects missing login payloads but accepts documented empty responses', async () => {
  const client = createClient({
    baseUrl: 'https://example.test/api',
    fetch: async () => new Response(null, { status: 204 }),
  })
  installApiInterceptors(client, { getToken: () => null, replaceToken: () => {}, logout: () => {} })
  await expect(
    exchangeTelegramLoginCode({ client, body: { code: 'code' }, throwOnError: true }),
  ).rejects.toBeDefined()
  await expect(
    deleteMySession({
      client,
      path: { id: '00000000-0000-4000-8000-000000000000' },
      throwOnError: true,
    }),
  ).resolves.toBeDefined()
})
