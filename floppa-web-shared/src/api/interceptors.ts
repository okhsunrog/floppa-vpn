import type { Client } from '../client/client'
import type { useAuthStore } from '../stores'

/** The slice of the auth store the interceptors need. */
type AuthSession = Pick<ReturnType<typeof useAuthStore>, 'getToken' | 'replaceToken' | 'logout'>

/** What the server puts in a 426 body; every field is optional because it came off the wire. */
export interface UpgradeRequiredBody {
  min_version?: string
  message?: string
}

export interface ApiInterceptorOptions {
  /** Sent as `X-Client-Version` on every request, so the server can refuse a build that is too old. */
  clientVersion?: string
  /** Called on a 426 with whatever the body carried. Only meaningful with `clientVersion`. */
  onUpgradeRequired?: (body: UpgradeRequiredBody) => void
}

function parseUpgradeRequiredBody(value: unknown): UpgradeRequiredBody {
  if (typeof value !== 'object' || value === null) return {}
  const { min_version, message } = value as Record<string, unknown>
  return {
    min_version: typeof min_version === 'string' ? min_version : undefined,
    message: typeof message === 'string' ? message : undefined,
  }
}

/**
 * Attach the session to the generated API client: bearer token on the way out; sliding-session
 * refresh, session death and forced upgrades on the way back.
 *
 * Deliberately no navigation here. Logging out flips `isAuthenticated`, and the router guard
 * installed by `installAuthGuard` is the one place that decides where a logged-out user goes.
 */
export function installApiInterceptors(
  client: Client,
  auth: AuthSession,
  options: ApiInterceptorOptions = {},
): void {
  const { clientVersion, onUpgradeRequired } = options

  client.interceptors.request.use((request) => {
    if (clientVersion) request.headers.set('X-Client-Version', clientVersion)
    const token = auth.getToken()
    if (token) request.headers.set('Authorization', `Bearer ${token}`)
    return request
  })

  client.interceptors.response.use(async (response, request) => {
    // Sliding session: the server attaches a fresh JWT once the current one is a day old.
    const refreshed = response.headers.get('x-refreshed-token')
    if (refreshed) auth.replaceToken(refreshed)

    // Only a rejected *authenticated* request means our session is dead; public endpoints
    // (e.g. a failed login-code exchange) also return 401 and must not wipe the session.
    if (response.status === 401 && request.headers.has('Authorization')) auth.logout()

    if (response.status === 426 && onUpgradeRequired) {
      let body: unknown = null
      try {
        body = await response.clone().json()
      } catch {
        // Not JSON — report an empty body; the caller picks its own fallback text.
      }
      onUpgradeRequired(parseUpgradeRequiredBody(body))
    }
    return response
  })
}
