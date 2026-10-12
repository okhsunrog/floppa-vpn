import { beforeEach, describe, expect, it, vi } from 'vite-plus/test'
const { invoke, isTauri } = vi.hoisted(() => ({ invoke: vi.fn(), isTauri: vi.fn(() => true) }))
vi.mock('@tauri-apps/api/core', () => ({ invoke, isTauri }))
import { nativeFetch } from './nativeFetch'

beforeEach(() => {
  invoke.mockReset()
  isTauri.mockReturnValue(true)
  invoke.mockImplementation(async (command) =>
    command === 'native_http'
      ? {
          status: 200,
          headers: [
            ['x-refreshed-token', 'new-token'],
            ['content-type', 'application/json'],
          ],
          body: [...new TextEncoder().encode('{"ok":true}')],
        }
      : undefined,
  )
})
describe('native API transport', () => {
  it('preserves auth headers, request body and refreshed session headers', async () => {
    const response = await nativeFetch('https://example.test/api/me', {
      method: 'POST',
      headers: { Authorization: 'Bearer token' },
      body: '{"x":1}',
    })
    const call = invoke.mock.calls.find(([command]) => command === 'native_http')!
    expect(call[1].request.headers).toContainEqual(['authorization', 'Bearer token'])
    expect(new TextDecoder().decode(new Uint8Array(call[1].request.body))).toBe('{"x":1}')
    expect(response.headers.get('x-refreshed-token')).toBe('new-token')
    expect(await response.json()).toEqual({ ok: true })
  })
  it('preserves HTTP failures as Responses for the generated SDK', async () => {
    invoke.mockResolvedValue({ status: 401, headers: [], body: [] })
    expect((await nativeFetch('https://example.test/api/me')).status).toBe(401)
  })
  it('rejects a cancelled request before sending it', async () => {
    const controller = new AbortController()
    controller.abort()
    await expect(
      nativeFetch('https://example.test/api/me', { signal: controller.signal }),
    ).rejects.toMatchObject({ name: 'AbortError' })
    expect(invoke).not.toHaveBeenCalled()
  })
  it('does not replay or follow redirects carrying authentication', async () => {
    invoke.mockResolvedValue({
      status: 307,
      headers: [['location', 'https://other.test']],
      body: [],
    })
    await expect(nativeFetch('https://example.test/api/me')).rejects.toBeInstanceOf(TypeError)
    expect(invoke.mock.calls.filter(([command]) => command === 'native_http')).toHaveLength(1)
  })
  it('supports empty 204 responses', async () => {
    invoke.mockResolvedValue({ status: 204, headers: [], body: [] })
    const response = await nativeFetch('https://example.test/api/me', { method: 'DELETE' })
    expect(response.status).toBe(204)
    expect(await response.text()).toBe('')
  })
  it('forwards cancellation while a native request is in flight', async () => {
    const controller = new AbortController()
    let rejectRequest!: (reason: unknown) => void
    let started!: () => void
    const sent = new Promise<void>((resolve) => {
      started = resolve
    })
    invoke.mockImplementation((command) => {
      if (command === 'native_http') {
        return new Promise((_, reject) => {
          rejectRequest = reject
          started()
        })
      }
      if (command === 'cancel_http') rejectRequest('request aborted')
      return Promise.resolve()
    })
    const response = nativeFetch('https://example.test/api/me', { signal: controller.signal })
    const rejected = expect(response).rejects.toMatchObject({ name: 'AbortError' })
    await sent
    controller.abort()
    await rejected
    const request = invoke.mock.calls.find(([command]) => command === 'native_http')![1].request
    expect(invoke).toHaveBeenCalledWith('cancel_http', { id: request.id })
  })
})
