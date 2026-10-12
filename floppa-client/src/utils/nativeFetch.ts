import { invoke, isTauri } from '@tauri-apps/api/core'
import { API_URL } from '../config'
import type { HttpRequest, HttpResponse } from '../bindings'

let configured: Promise<void> | undefined

/** Fetch subset used by our generated SDK: buffered bodies, bearer auth, no redirects/cookies. */
export const nativeFetch: typeof fetch = async (input, init) => {
  if (!isTauri()) return globalThis.fetch(input, init)
  const request = new Request(input, init)
  if (request.signal.aborted) throw new DOMException('Request aborted', 'AbortError')
  const id = crypto.randomUUID()
  const abort = () => {
    void invoke('cancel_http', { id }).catch(() => {})
  }
  request.signal.addEventListener('abort', abort, { once: true })
  try {
    configured ??= invoke<void>('configure_http', { base: API_URL }).catch((error) => {
      configured = undefined
      throw error
    })
    await configured
    const body = request.body ? Array.from(new Uint8Array(await request.arrayBuffer())) : null
    if (request.signal.aborted) throw new DOMException('Request aborted', 'AbortError')
    const result = await invoke<HttpResponse>('native_http', {
      request: {
        id,
        url: request.url,
        method: request.method,
        headers: Array.from(request.headers.entries()),
        body,
      } satisfies HttpRequest,
    })
    if (request.signal.aborted) throw new DOMException('Request aborted', 'AbortError')
    // Never silently replay POSTs or forward credentials to a redirect destination.
    if (result.status >= 300 && result.status < 400) throw new TypeError('Unexpected API redirect')
    const response = new Response(
      [204, 205, 304].includes(result.status) || request.method === 'HEAD'
        ? null
        : new Uint8Array(result.body),
      {
        status: result.status,
        headers: result.headers,
      },
    )
    Object.defineProperty(response, 'url', { value: request.url })
    return response
  } catch (error) {
    if (request.signal.aborted) throw new DOMException('Request aborted', 'AbortError')
    throw new TypeError(typeof error === 'string' ? error : 'Network request failed', {
      cause: error,
    })
  } finally {
    request.signal.removeEventListener('abort', abort)
  }
}
