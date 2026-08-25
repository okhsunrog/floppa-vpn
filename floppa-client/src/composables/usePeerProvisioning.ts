import { computed, reactive, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getMeQuery } from 'floppa-web-shared/client/@pinia/colada.gen'
import {
  createMyPeer,
  getMyPeerByDevice,
  getMyPeerConfig,
  getMyVlessConfig,
  getPublicConfig,
  upsertMyInstallation,
} from 'floppa-web-shared/client/sdk.gen'
import type {
  CreatePeerRequest,
  CreatePeerResponse,
  MyPeer,
  PublicConfig,
  UpsertInstallationRequest,
  VlessConfigResponse,
} from 'floppa-web-shared/client/types.gen'
import { describeError, isApiError, type TranslateKey } from 'floppa-web-shared/utils'
import { platform } from '@tauri-apps/plugin-os'
import type { CycleOutcome, Protocol } from '../bindings'
import { useVpnStore } from '../stores/vpnStore'
import type { VpnError } from '../utils/vpnErrors'

/*
 * Server-side peer provisioning for this device.
 *
 * Everything that talks to the server is a plain function over `ProvisioningApi` and
 * `ProvisioningDeps`, so it can be exercised with fakes; the composable at the bottom is the only
 * part that knows about the stores and the request cache. Owned by the connection card rather
 * than a store because none of this is tunnel state — the tunnel is genuinely idle while a peer
 * is being provisioned — and nothing outside the card reads it.
 */

/** The protocols that are backed by a per-device peer row on the server (VLESS is per-user). */
export type WgFamilyProtocol = Exclude<Protocol, 'vless'>

/** Locale keys a failed server sync can be reported under. */
export type SyncErrorKey = 'vpn.noSubscription' | 'vpn.peerLimitReached' | 'vpn.peerCreateFailed'

export type SyncResult =
  | { outcome: 'ok' }
  | { outcome: 'error'; errorKey: SyncErrorKey; detail?: string }
  | { outcome: 'offline' }

/**
 * Whether this device has no peer for a protocol, as opposed to us failing to find out.
 *
 * Only a 404 means "no peer". A network failure leaves `data` empty too, and reading that as
 * "no peer" is what used to make an offline start create a duplicate — or, in the reconnect
 * path, re-provision a peer that was never gone.
 */
export type PeerLookup = { found: 'yes'; id: number } | { found: 'no' } | { found: 'unknown' }

/** What a peer lookup / create call hands back, in the shape the generated client uses. */
export interface ApiCallResult<T> {
  data?: T
  error?: unknown
  /** Absent when no server answered at all. */
  response?: { status: number }
}

/**
 * The slice of the generated SDK the provisioning talks to.
 *
 * Narrowed to what is actually read from each call, so a test can hand in plain async functions
 * instead of re-creating hey-api's generic result types. `sdkProvisioningApi` binds the real
 * client.
 */
export interface ProvisioningApi {
  getMyPeerByDevice(deviceId: string, protocol: WgFamilyProtocol): Promise<ApiCallResult<MyPeer>>
  /** Throws on any failure — a peer we just found must have a config. */
  getMyPeerConfig(id: number): Promise<string>
  createMyPeer(body: CreatePeerRequest): Promise<ApiCallResult<CreatePeerResponse>>
  getMyVlessConfig(): Promise<ApiCallResult<VlessConfigResponse>>
  getPublicConfig(): Promise<ApiCallResult<PublicConfig>>
  upsertMyInstallation(body: UpsertInstallationRequest): Promise<unknown>
}

export function sdkProvisioningApi(): ProvisioningApi {
  return {
    getMyPeerByDevice: (device_id, protocol) =>
      getMyPeerByDevice({ path: { device_id }, query: { protocol } }),
    getMyPeerConfig: async (id) =>
      (await getMyPeerConfig({ path: { id }, throwOnError: true })).data,
    // Not the Pinia Colada mutation: it re-throws the error body untyped, and `response` — the
    // only thing that tells a server refusal from no server at all — is lost on the way.
    createMyPeer: (body) => createMyPeer({ body, throwOnError: false }),
    getMyVlessConfig: () => getMyVlessConfig(),
    getPublicConfig: () => getPublicConfig(),
    upsertMyInstallation: (body) => upsertMyInstallation({ body }),
  }
}

/** Everything one sync needs from the app besides the API: identity, the store, the cache. */
export interface ProvisioningDeps {
  api: ProvisioningApi
  deviceId: string
  deviceName: string | null
  platform: string
  appVersion: string
  /** Re-read `/me`; `false` when the server could not be reached. */
  refreshMe(): Promise<boolean>
  hasSubscription(): boolean
  importConfig(raw: string): Promise<void>
  t: TranslateKey
}

export async function lookupPeer(
  api: ProvisioningApi,
  deviceId: string,
  protocol: WgFamilyProtocol,
): Promise<PeerLookup> {
  const { data: peer, response } = await api.getMyPeerByDevice(deviceId, protocol)
  if (peer) return { found: 'yes', id: peer.id }
  return response?.status === 404 ? { found: 'no' } : { found: 'unknown' }
}

/**
 * Fetch (and optionally create) the wg-family peer for `protocol`, loading its config into the
 * VPN store. `allowCreate=false` only loads a pre-existing peer (so the secondary protocol never
 * consumes a peer slot). Returns an error outcome on subscription/limit failures during create.
 */
export async function syncWgFamilyPeer(
  deps: ProvisioningDeps,
  protocol: WgFamilyProtocol,
  allowCreate: boolean,
): Promise<SyncResult> {
  const lookup = await lookupPeer(deps.api, deps.deviceId, protocol)

  if (lookup.found === 'yes') {
    await deps.importConfig(await deps.api.getMyPeerConfig(lookup.id))
    return { outcome: 'ok' }
  }

  if (lookup.found === 'unknown') return { outcome: 'offline' }

  if (!allowCreate) return { outcome: 'ok' }

  if (!deps.hasSubscription()) {
    return { outcome: 'error', errorKey: 'vpn.noSubscription' }
  }

  const {
    data: created,
    error,
    response,
  } = await deps.api.createMyPeer({
    device_id: deps.deviceId,
    device_name: deps.deviceName,
    protocol,
  })
  if (created) {
    await deps.importConfig(created.config)
    return { outcome: 'ok' }
  }
  if (!response) return { outcome: 'offline' }

  // Error codes as `floppa-server/src/admin/error.rs` names them. `isApiError` narrows the code
  // to the `ApiErrorCode` union, so a misspelled case here is a type error rather than a branch
  // that never matches.
  if (isApiError(error)) {
    switch (error.error) {
      case 'no_active_subscription':
        return { outcome: 'error', errorKey: 'vpn.noSubscription' }
      case 'peer_limit_reached':
        return { outcome: 'error', errorKey: 'vpn.peerLimitReached' }
    }
  }
  return {
    outcome: 'error',
    errorKey: 'vpn.peerCreateFailed',
    detail: describeError(error, `HTTP ${response.status}`, deps.t),
  }
}

/**
 * One full sync: register the installation, provision the wg-family peers, fetch the VLESS
 * config. Never throws — anything unexpected is reported as `offline`.
 */
export async function syncPeers(deps: ProvisioningDeps): Promise<SyncResult> {
  try {
    // If the server is unreachable, refreshMe silently fails (Pinia Colada doesn't throw).
    // Otherwise getMyPeerByDevice returns { data: undefined } on network error, which looks
    // identical to a 404 and would wrongly revoke cached config.
    if (!(await deps.refreshMe())) return { outcome: 'offline' }

    try {
      await deps.api.upsertMyInstallation({
        device_id: deps.deviceId,
        device_name: deps.deviceName ?? undefined,
        platform: deps.platform,
        app_version: deps.appVersion,
      })
    } catch {
      // Non-critical — continue with peer sync even if installation upsert fails
    }

    // AmneziaWG is the default wg-family protocol when the server offers it; WireGuard otherwise.
    let amneziaAvailable = false
    try {
      const { data: pub } = await deps.api.getPublicConfig()
      amneziaAvailable = pub?.amneziawg_available ?? false
    } catch {
      // Couldn't reach /config — fall back to plain WireGuard.
    }
    const primary: WgFamilyProtocol = amneziaAvailable ? 'amneziawg' : 'wireguard'
    const secondary: WgFamilyProtocol = primary === 'amneziawg' ? 'wireguard' : 'amneziawg'

    // 1. Provision the primary (default) wg-family peer — must succeed.
    const primaryResult = await syncWgFamilyPeer(deps, primary, true)
    if (primaryResult.outcome === 'error') return primaryResult

    // 2. Also provision the secondary wg-family protocol when the server offers it. A device is a
    //    single peer-limit slot, so holding both WireGuard and AmneziaWG is free — this gives the
    //    user all switcher positions. Best-effort: don't fail the sync if the bonus peer can't be
    //    made. The secondary wg protocol is only ever available when AmneziaWG is offered: if it
    //    isn't, the primary is WireGuard and the secondary would be the absent AmneziaWG.
    await syncWgFamilyPeer(deps, secondary, amneziaAvailable)

    // 3. Fetch VLESS config (per-user, no peer slot). A server that does not offer VLESS says so
    //    with `vless_not_configured`, which is not a failure of ours; anything else is worth a
    //    log line but must not fail the sync — the wg-family peer above is what matters.
    try {
      const { data: vlessConfig, error: vlessError } = await deps.api.getMyVlessConfig()
      if (vlessConfig?.uri) {
        await deps.importConfig(vlessConfig.uri)
      } else if (isApiError(vlessError) && vlessError.error !== 'vless_not_configured') {
        console.warn('[provisioning] VLESS config refused:', vlessError.error, vlessError.message)
      }
    } catch (e) {
      console.warn('[provisioning] VLESS config unavailable:', e)
    }

    // Importing configs does not change which protocol a connect would use, so there is nothing
    // to restore here.
    return { outcome: 'ok' }
  } catch {
    return { outcome: 'offline' }
  }
}

/** How the card reports the last sync: quietly, with an error, or as "offline". */
export type SetupPhase = 'idle' | 'offline'

export interface SetupState {
  phase: SetupPhase
  errorKey: SyncErrorKey | null
  errorDetail: string | null
}

export function emptySetupState(): SetupState {
  return { phase: 'idle', errorKey: null, errorDetail: null }
}

export function applySyncResult(state: SetupState, result: SyncResult) {
  switch (result.outcome) {
    case 'ok':
      state.phase = 'idle'
      break
    case 'error':
      state.phase = 'idle'
      state.errorKey = result.errorKey
      state.errorDetail = result.detail ?? null
      break
    case 'offline':
      state.phase = 'offline'
      break
  }
}

/** How long a sync may take before the card shows the offline banner and lets it finish behind. */
export const SYNC_TIMEOUT_MS = 5000

/**
 * Runs syncs against `state` one at a time, in the sense that only the latest one may write.
 *
 * A sync that outlives the timeout is not abandoned: the banner goes up, and if the sync later
 * finishes with a verdict it is applied — unless a newer run has started since, or the user's
 * retry already took the banner down.
 */
export function createSyncSequencer(state: SetupState, timeoutMs = SYNC_TIMEOUT_MS) {
  let generation = 0

  async function run(sync: Promise<SyncResult>): Promise<void> {
    state.errorKey = null
    state.errorDetail = null

    const thisGeneration = ++generation
    const isCurrent = () => thisGeneration === generation

    const timeout = new Promise<{ type: 'timeout' }>((resolve) =>
      setTimeout(() => resolve({ type: 'timeout' }), timeoutMs),
    )
    const winner = await Promise.race([
      sync.then((result) => ({ type: 'sync' as const, result })),
      timeout,
    ])

    if (!isCurrent()) return

    if (winner.type === 'sync') {
      applySyncResult(state, winner.result)
      return
    }

    state.phase = 'offline'
    void sync.then((result) => {
      if (!isCurrent()) return
      if (state.phase !== 'offline') return
      if (result.outcome !== 'offline') applySyncResult(state, result)
    })
  }

  return { run }
}

/** What a cycle that ended without a tunnel asks of the card. */
export type OutcomeAction =
  | { action: 'ignore' }
  | { action: 'show_error'; error: VpnError }
  /** The peer for `protocol` may have been deleted server-side: check, and recreate it if so. */
  | { action: 'reprovision'; protocol: WgFamilyProtocol }

/**
 * Decide what a finished cycle means.
 *
 * The one thing this cannot do is decide *why* it failed — that comes typed from the actor. A
 * protocol whose verification failed is the signal that its peer may have been deleted
 * server-side, and it is looked up by name rather than by "whichever protocol was tried last",
 * which is what the old code assumed and got wrong whenever the order had more than one entry.
 */
export function planOutcomeResponse(outcome: CycleOutcome): OutcomeAction {
  if (outcome.outcome === 'unwind_failed') {
    return { action: 'show_error', error: { kind: 'unwind_failed' } }
  }

  const verifyFailed =
    outcome.outcome === 'exhausted'
      ? outcome.failures.find((f) => f.error.kind === 'verify_failed')?.protocol
      : outcome.outcome === 'lost_gave_up'
        ? outcome.protocol
        : undefined

  // Nothing to re-provision: the probes failed for reasons a new peer would not fix. Show the
  // last probe's typed error — it is the one for the protocol the user most likely cares about,
  // and every kind it can carry has words in the locale.
  if (outcome.outcome === 'exhausted' && !verifyFailed) {
    const failure = outcome.failures.at(-1)
    if (failure && failure.error.kind !== 'cancelled') {
      return { action: 'show_error', error: { kind: 'attempt_failed', failure } }
    }
    return { action: 'ignore' }
  }

  // VLESS has no per-device peer to look up: its config is per-user and never deleted by a
  // peer removal, so a failed VLESS verification is not a "peer gone" signal.
  if (verifyFailed === 'vless') {
    return { action: 'show_error', error: { kind: 'connection_failed' } }
  }
  if (!verifyFailed) return { action: 'ignore' }
  return { action: 'reprovision', protocol: verifyFailed }
}

export type ReprovisionOutcome =
  /** The peer was gone; a new one was provisioned and a connect was requested. */
  | 'reconnected'
  /** The peer was gone and re-provisioning left us without any config. */
  | 'no_config'
  /** The peer still exists — the failure is elsewhere. */
  | 'peer_exists'
  /** The server could not be asked. */
  | 'unreachable'

export interface ReprovisionDeps {
  lookup(): Promise<PeerLookup>
  /** A full sync (`syncPeers` through the sequencer). */
  resync(): Promise<void>
  hasConfig(): boolean
  reconnect(): Promise<void>
}

/** After a verification failure: find out whether the peer is gone, and if so replace it once. */
export async function reprovisionPeer(deps: ReprovisionDeps): Promise<ReprovisionOutcome> {
  console.info('[provisioning] checking whether the peer still exists on the server...')
  const lookup = await deps.lookup()
  switch (lookup.found) {
    case 'no':
      console.info('[provisioning] peer is gone, recreating it')
      await deps.resync()
      if (!deps.hasConfig()) return 'no_config'
      console.info('[provisioning] got a new config, reconnecting')
      await deps.reconnect()
      return 'reconnected'
    case 'yes':
      console.info('[provisioning] the peer exists, so the problem is elsewhere')
      return 'peer_exists'
    case 'unknown':
      console.warn('[provisioning] could not reach the server to check the peer')
      return 'unreachable'
  }
}

/**
 * The card's view of provisioning: the offline banner, the setup error, the "busy talking to
 * the server" flag, and the two entry points — the sync on mount/retry and the reaction to a
 * failed cycle.
 */
export function usePeerProvisioning() {
  const { t } = useI18n()
  const vpn = useVpnStore()
  const api = sdkProvisioningApi()

  const { data: me, refresh: refreshMe, error: meQueryError } = useQuery(getMeQuery())

  const setup = reactive(emptySetupState())
  const sequencer = createSyncSequencer(setup)

  /**
   * True while we are talking to the server about re-provisioning a peer. Not a tunnel state:
   * the tunnel is genuinely idle during it, which is why the store does not know about it.
   */
  const reprovisioning = ref(false)

  const setupPhase = computed(() => setup.phase)
  const setupError = computed<string | null>(() =>
    setup.errorKey ? t(setup.errorKey, { detail: setup.errorDetail ?? '' }) : null,
  )

  /** Clear the offline banner once the server answers `/me` again. */
  function noteServerReachable() {
    if (setup.phase === 'offline') setup.phase = 'idle'
  }

  function deps(deviceId: string): ProvisioningDeps {
    return {
      api,
      deviceId,
      deviceName: vpn.deviceName,
      platform: platform(),
      appVersion: __APP_VERSION__,
      refreshMe: async () => {
        await refreshMe()
        return !meQueryError.value
      },
      hasSubscription: () => !!me.value?.subscription,
      importConfig: (raw) => vpn.importConfig(raw),
      t,
    }
  }

  /** Provision this device's peers, showing the offline banner if the server is slow. */
  async function setupAutoPeer(): Promise<void> {
    const deviceId = vpn.deviceId
    if (!deviceId) return
    await sequencer.run(syncPeers(deps(deviceId)))
  }

  /** React to a cycle that ended without connecting. */
  async function handleOutcome(outcome: CycleOutcome | null): Promise<void> {
    if (!outcome) return
    const plan = planOutcomeResponse(outcome)
    if (plan.action === 'ignore') return
    if (plan.action === 'show_error') {
      vpn.setError(plan.error)
      return
    }

    const deviceId = vpn.deviceId
    if (!deviceId) return

    reprovisioning.value = true
    try {
      const result = await reprovisionPeer({
        lookup: () => lookupPeer(api, deviceId, plan.protocol),
        resync: setupAutoPeer,
        hasConfig: () => vpn.hasConfig,
        reconnect: async () => void (await vpn.connect()),
      })
      if (result === 'peer_exists' || result === 'unreachable') {
        vpn.setError({ kind: 'connection_failed' })
      }
    } catch (e) {
      console.error('[provisioning] peer check failed:', e)
      vpn.setError({ kind: 'connection_failed' })
    } finally {
      reprovisioning.value = false
    }
  }

  return {
    setupPhase,
    setupError,
    reprovisioning,
    meQueryError,
    noteServerReachable,
    setupAutoPeer,
    handleOutcome,
  }
}
