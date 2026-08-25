/**
 * `error` codes the server puts in its error body (`floppa-server/src/admin/error.rs`).
 * A union rather than `string` so that comparing against a misspelled code is a type error
 * instead of a check that silently never matches.
 */
export type ApiErrorCode =
  // ApiError constructors
  | 'bad_gateway'
  | 'bad_request'
  | 'conflict'
  | 'forbidden'
  | 'internal_error'
  | 'not_found'
  | 'too_many_requests'
  | 'unauthorized'
  // FloppaError / sqlx conversions
  | 'amneziawg_not_configured'
  | 'database_error'
  | 'invalid_credentials'
  | 'invalid_installation'
  | 'invalid_login'
  | 'invalid_password'
  | 'login_taken'
  | 'no_active_subscription'
  | 'no_available_ips'
  | 'peer_already_exists'
  | 'peer_limit_reached'
  | 'vless_not_configured'

/** The JSON body the server returns for every non-2xx response. */
export interface ApiError {
  error: ApiErrorCode
  message: string
}

/**
 * The generated client (with `throwOnError`) rejects with the *parsed response body*, not an
 * `Error` and not a response: for our server that is `{ error, message }`. Nothing on it carries
 * the HTTP status, so `(e as { status }).status === 409` never matches — compare `error` codes.
 */
export function isApiError(e: unknown): e is ApiError {
  if (typeof e !== 'object' || e === null) return false
  const { error, message } = e as Record<string, unknown>
  return typeof error === 'string' && typeof message === 'string'
}

/**
 * Locale keys for the codes whose server message is not what a user should read: the message is
 * English and written for a log line, while these are the refusals a non-admin user can actually
 * run into and needs to understand in their own language. Everything else keeps the server text.
 */
export const API_ERROR_MESSAGE_KEYS: Partial<Record<ApiErrorCode, string>> = {
  forbidden: 'apiErrors.forbidden',
  unauthorized: 'apiErrors.unauthorized',
  too_many_requests: 'apiErrors.tooManyRequests',
  no_active_subscription: 'apiErrors.noActiveSubscription',
  peer_limit_reached: 'apiErrors.peerLimitReached',
  vless_not_configured: 'apiErrors.vlessNotConfigured',
  amneziawg_not_configured: 'apiErrors.amneziawgNotConfigured',
}

/** The slice of vue-i18n's `t` that `describeError` needs. */
export type TranslateKey = (key: string) => string

/**
 * User-facing text for a failed request: the server's message when the failure is an API error
 * (peer limit reached, plan in use, …), otherwise `fallback`. Anything else — a network
 * `TypeError` ("Failed to fetch"), a proxy's HTML page, a bare string — carries nothing worth
 * showing, so the caller's translated fallback wins.
 *
 * With a translator, codes listed in `API_ERROR_MESSAGE_KEYS` are worded from the locale instead
 * of the server's English message.
 */
export function describeError(e: unknown, fallback: string, t?: TranslateKey): string {
  if (!isApiError(e)) return fallback
  const key = t && API_ERROR_MESSAGE_KEYS[e.error]
  return key ? t(key) : e.message
}
