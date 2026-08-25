import type {
  AttemptError,
  AttemptFailure,
  BackendError,
  ConfigError,
  IntentError,
  StepKind,
} from '../bindings'

/**
 * Errors that are not about the tunnel: a request the actor refused, a config it could not
 * store, a call that never reached it — plus what the card concludes after a failed cycle.
 *
 * A union keyed on `kind`, never a string: the Rust halves arrive typed, and `describeVpnError`
 * maps every kind to locale text through Records over the unions, so a variant added in Rust
 * fails to compile here until it has been given words.
 */
export type VpnError =
  | IntentError
  | ConfigError
  /** A cycle ended without a tunnel; `failure` is the probe whose error is worth showing. */
  | { kind: 'attempt_failed'; failure: AttemptFailure }
  /** A teardown could not be confirmed and the machine may be left dirty. */
  | { kind: 'unwind_failed' }
  | { kind: 'connection_failed' }
  | { kind: 'unexpected'; detail: string }

export type VpnErrorKind = VpnError['kind']

/** Locale key for each error kind; `{detail}` is interpolated where the kind carries one. */
export const VPN_ERROR_KEYS: Record<Exclude<VpnErrorKind, 'attempt_failed'>, string> = {
  empty_order: 'vpn.errors.emptyOrder',
  no_usable_config: 'vpn.errors.noUsableConfig',
  actor_gone: 'vpn.errors.actorGone',
  settle_timeout: 'vpn.errors.settleTimeout',
  empty: 'vpn.errors.emptyConfig',
  unparseable: 'vpn.errors.unparseableConfig',
  unwind_failed: 'vpn.errors.unwindFailed',
  connection_failed: 'vpn.connectionFailed',
  unexpected: 'vpn.errors.unexpected',
}

/** Why one probe failed, as reported by the actor. */
export const ATTEMPT_ERROR_KEYS: Record<AttemptError['kind'], string> = {
  permission_denied: 'vpn.errors.attempt.permissionDenied',
  consent_unavailable: 'vpn.errors.attempt.consentUnavailable',
  no_config: 'vpn.errors.attempt.noConfig',
  platform_unavailable: 'vpn.errors.attempt.platformUnavailable',
  resolve_failed: 'vpn.errors.attempt.resolveFailed',
  invalid_config: 'vpn.errors.attempt.invalidConfig',
  platform: 'vpn.errors.attempt.platform',
  backend: 'vpn.errors.attempt.backend',
  verify_failed: 'vpn.errors.attempt.verifyFailed',
  timed_out: 'vpn.errors.attempt.timedOut',
  peer_start_failed: 'vpn.errors.attempt.peerStartFailed',
  cancelled: 'vpn.errors.attempt.cancelled',
  crashed: 'vpn.errors.attempt.crashed',
}

/** What the tunnel engine reported, nested inside an `AttemptError` of kind `backend`. */
export const BACKEND_ERROR_KEYS: Record<BackendError['kind'], string> = {
  permission_denied: 'vpn.errors.backend.permissionDenied',
  invalid_config: 'vpn.errors.backend.invalidConfig',
  engine: 'vpn.errors.backend.engine',
  not_running: 'vpn.errors.backend.notRunning',
  service_unreachable: 'vpn.errors.backend.serviceUnreachable',
  service_refused: 'vpn.errors.backend.serviceRefused',
  unsupported: 'vpn.errors.backend.unsupported',
}

/** The platform step that failed, for `AttemptError` of kind `platform`. */
export const STEP_KEYS: Record<StepKind, string> = {
  prepare_link: 'vpn.errors.step.prepareLink',
  start_backend: 'vpn.errors.step.startBackend',
  address: 'vpn.errors.step.address',
  endpoint_route: 'vpn.errors.step.endpointRoute',
  routes: 'vpn.errors.step.routes',
  dns: 'vpn.errors.step.dns',
  android_service: 'vpn.errors.step.androidService',
}

/** The slice of vue-i18n's `t` these need. */
export type Translate = (key: string, params?: Record<string, unknown>) => string

export function describeBackendError(error: BackendError, t: Translate): string {
  return t(BACKEND_ERROR_KEYS[error.kind], 'detail' in error ? { detail: error.detail } : {})
}

export function describeAttemptError(error: AttemptError, t: Translate): string {
  const key = ATTEMPT_ERROR_KEYS[error.kind]
  switch (error.kind) {
    case 'no_config':
      return t(key, { protocol: t(`vpn.${error.protocol}`) })
    case 'resolve_failed':
      return t(key, { host: error.host, detail: error.detail })
    case 'platform':
      return t(key, { step: t(STEP_KEYS[error.step]), detail: error.detail })
    case 'backend':
      return t(key, { detail: describeBackendError(error.error, t) })
    default:
      return t(key, 'detail' in error ? { detail: error.detail } : {})
  }
}

/** The one place a `VpnError` becomes text. */
export function describeVpnError(error: VpnError, t: Translate): string {
  if (error.kind === 'attempt_failed') {
    return t('vpn.errors.attemptFailed', {
      protocol: t(`vpn.${error.failure.protocol}`),
      reason: describeAttemptError(error.failure.error, t),
    })
  }
  return t(VPN_ERROR_KEYS[error.kind], 'detail' in error ? { detail: error.detail } : {})
}
