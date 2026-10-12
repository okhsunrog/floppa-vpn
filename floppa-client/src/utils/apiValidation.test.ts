import { describe, expect, it } from 'vite-plus/test'
import { createClient } from 'floppa-web-shared/client/client'
import { exchangeTelegramLoginCode } from 'floppa-web-shared/client/sdk.gen'
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
