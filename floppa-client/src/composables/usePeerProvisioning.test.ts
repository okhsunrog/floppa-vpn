import { afterEach, beforeEach, describe, expect, it, vi } from 'vite-plus/test'
import type { Protocol } from '../bindings'
import {
  applySyncResult,
  createSyncSequencer,
  emptySetupState,
  lookupPeer,
  planOutcomeResponse,
  planWithoutReprovision,
  reprovisionPeer,
  syncPeers,
  syncWgFamilyPeer,
  type ProvisioningApi,
  type ProvisioningDeps,
  type SetupState,
  type SyncResult,
  type WgFamilyProtocol,
} from './usePeerProvisioning'

/** A server holding `peers` for this device, answering 404 for everything else. */
function fakeApi(
  peers: Partial<Record<WgFamilyProtocol, { id: number; config: string }>> = {},
  overrides: Partial<ProvisioningApi> = {},
): ProvisioningApi & { calls: string[] } {
  const calls: string[] = []
  return {
    calls,
    async getMyPeerByDevice(_deviceId, protocol) {
      calls.push(`lookup:${protocol}`)
      const peer = peers[protocol]
      return peer
        ? {
            data: {
              id: peer.id,
              protocol,
              assigned_ip: '10.0.0.2',
              created_at: '',
              download_bytes: 0,
              upload_bytes: 0,
              public_key: '',
              sync_status: 'active',
            },
            response: { status: 200 },
          }
        : { response: { status: 404 } }
    },
    async getMyPeerConfig(id) {
      calls.push(`config:${id}`)
      return Object.values(peers).find((p) => p.id === id)?.config ?? ''
    },
    async createMyPeer(body) {
      calls.push(`create:${body.protocol}`)
      return { data: { id: 99, assigned_ip: '10.0.0.9', config: `created-${body.protocol}` } }
    },
    async getMyVlessConfig() {
      calls.push('vless')
      return { error: { error: 'vless_not_configured', message: 'no' }, response: { status: 404 } }
    },
    async getPublicConfig() {
      calls.push('public')
      return { data: { amneziawg_available: true, vless_available: false } }
    },
    async upsertMyInstallation() {
      calls.push('installation')
      return {}
    },
    ...overrides,
  }
}

function fakeDeps(api: ProvisioningApi, overrides: Partial<ProvisioningDeps> = {}) {
  const imported: string[] = []
  const deps: ProvisioningDeps = {
    api,
    deviceId: 'dev-1',
    deviceName: 'Phone',
    platform: 'android',
    appVersion: '1.2.3',
    refreshMe: async () => true,
    hasSubscription: () => true,
    importConfig: async (raw) => void imported.push(raw),
    t: (key) => `t:${key}`,
    ...overrides,
  }
  return { deps, imported }
}

describe('lookupPeer', () => {
  it('distinguishes a missing peer from an unreachable server', async () => {
    const api = fakeApi({ wireguard: { id: 7, config: 'wg' } })
    expect(await lookupPeer(api, 'dev-1', 'wireguard')).toEqual({ found: 'yes', id: 7 })
    expect(await lookupPeer(api, 'dev-1', 'amneziawg')).toEqual({ found: 'no' })

    const offline = fakeApi({}, { getMyPeerByDevice: async () => ({}) })
    expect(await lookupPeer(offline, 'dev-1', 'wireguard')).toEqual({ found: 'unknown' })

    const serverError = fakeApi(
      {},
      { getMyPeerByDevice: async () => ({ response: { status: 500 } }) },
    )
    expect(await lookupPeer(serverError, 'dev-1', 'wireguard')).toEqual({ found: 'unknown' })
  })
})

describe('syncWgFamilyPeer', () => {
  it('adopts an existing peer without creating one', async () => {
    const api = fakeApi({ amneziawg: { id: 3, config: 'awg-conf' } })
    const { deps, imported } = fakeDeps(api)

    expect(await syncWgFamilyPeer(deps, 'amneziawg', true)).toEqual({ outcome: 'ok' })
    expect(imported).toEqual(['awg-conf'])
    expect(api.calls).toEqual(['lookup:amneziawg', 'config:3'])
  })

  it('creates the peer when it is missing and creation is allowed', async () => {
    const api = fakeApi()
    const { deps, imported } = fakeDeps(api)

    expect(await syncWgFamilyPeer(deps, 'wireguard', true)).toEqual({ outcome: 'ok' })
    expect(imported).toEqual(['created-wireguard'])
    expect(api.calls).toEqual(['lookup:wireguard', 'create:wireguard'])
  })

  it('never creates when told not to, and reports ok', async () => {
    const api = fakeApi()
    const { deps, imported } = fakeDeps(api)

    expect(await syncWgFamilyPeer(deps, 'wireguard', false)).toEqual({ outcome: 'ok' })
    expect(imported).toEqual([])
    expect(api.calls).toEqual(['lookup:wireguard'])
  })

  it('is offline when the lookup got no answer', async () => {
    const api = fakeApi({}, { getMyPeerByDevice: async () => ({}) })
    const { deps } = fakeDeps(api)
    expect(await syncWgFamilyPeer(deps, 'wireguard', true)).toEqual({ outcome: 'offline' })
    expect(api.calls).toEqual([])
  })

  it('refuses to create without a subscription, before asking the server', async () => {
    const api = fakeApi()
    const { deps } = fakeDeps(api, { hasSubscription: () => false })
    expect(await syncWgFamilyPeer(deps, 'wireguard', true)).toEqual({
      outcome: 'error',
      errorKey: 'vpn.noSubscription',
    })
    expect(api.calls).toEqual(['lookup:wireguard'])
  })

  it('maps the server refusals by ApiErrorCode and keeps the rest as detail', async () => {
    const refusal = (error: string, status: number) =>
      fakeApi(
        {},
        { createMyPeer: async () => ({ error: { error, message: 'msg' }, response: { status } }) },
      )

    const limit = fakeDeps(refusal('peer_limit_reached', 403)).deps
    expect(await syncWgFamilyPeer(limit, 'wireguard', true)).toEqual({
      outcome: 'error',
      errorKey: 'vpn.peerLimitReached',
    })

    const noSub = fakeDeps(refusal('no_active_subscription', 402)).deps
    expect(await syncWgFamilyPeer(noSub, 'wireguard', true)).toEqual({
      outcome: 'error',
      errorKey: 'vpn.noSubscription',
    })

    // An unlisted code keeps the server's message as the detail
    const other = fakeDeps(refusal('no_available_ips', 500)).deps
    expect(await syncWgFamilyPeer(other, 'wireguard', true)).toEqual({
      outcome: 'error',
      errorKey: 'vpn.peerCreateFailed',
      detail: 'msg',
    })

    // A non-API body (a proxy page, say) falls back to the HTTP status
    const html = fakeApi(
      {},
      { createMyPeer: async () => ({ error: '<html>', response: { status: 502 } }) },
    )
    expect(await syncWgFamilyPeer(fakeDeps(html).deps, 'wireguard', true)).toEqual({
      outcome: 'error',
      errorKey: 'vpn.peerCreateFailed',
      detail: 'HTTP 502',
    })
  })

  it('is offline when the create call never reached a server', async () => {
    const api = fakeApi({}, { createMyPeer: async () => ({ error: new TypeError('fetch') }) })
    expect(await syncWgFamilyPeer(fakeDeps(api).deps, 'wireguard', true)).toEqual({
      outcome: 'offline',
    })
  })
})

describe('syncPeers', () => {
  it('provisions AmneziaWG first and WireGuard as a free extra when the server offers AWG', async () => {
    const api = fakeApi()
    const { deps, imported } = fakeDeps(api)

    expect(await syncPeers(deps)).toEqual({ outcome: 'ok' })
    expect(api.calls).toEqual([
      'installation',
      'public',
      'lookup:amneziawg',
      'create:amneziawg',
      'lookup:wireguard',
      'create:wireguard',
      'vless',
    ])
    expect(imported).toEqual(['created-amneziawg', 'created-wireguard'])
  })

  it('only ever creates WireGuard when AmneziaWG is not offered', async () => {
    const api = fakeApi(
      {},
      {
        getPublicConfig: async () => ({
          data: { amneziawg_available: false, vless_available: false },
        }),
      },
    )
    const { deps } = fakeDeps(api)

    expect(await syncPeers(deps)).toEqual({ outcome: 'ok' })
    expect(api.calls.filter((c) => c.startsWith('create:'))).toEqual(['create:wireguard'])
    expect(api.calls).toContain('lookup:amneziawg')
  })

  it('imports the VLESS config when the server has one', async () => {
    const api = fakeApi(
      { amneziawg: { id: 1, config: 'awg' }, wireguard: { id: 2, config: 'wg' } },
      { getMyVlessConfig: async () => ({ data: { uri: 'vless://x' } }) },
    )
    const { deps, imported } = fakeDeps(api)
    expect(await syncPeers(deps)).toEqual({ outcome: 'ok' })
    expect(imported).toEqual(['awg', 'wg', 'vless://x'])
  })

  it('is offline when /me cannot be refreshed, before touching anything else', async () => {
    const api = fakeApi()
    const { deps } = fakeDeps(api, { refreshMe: async () => false })
    expect(await syncPeers(deps)).toEqual({ outcome: 'offline' })
    expect(api.calls).toEqual([])
  })

  it('propagates a failure of the primary peer and does not go on', async () => {
    const api = fakeApi()
    const { deps } = fakeDeps(api, { hasSubscription: () => false })
    expect(await syncPeers(deps)).toEqual({ outcome: 'error', errorKey: 'vpn.noSubscription' })
    expect(api.calls.at(-1)).toBe('lookup:amneziawg')
  })

  it('survives an installation upsert or /config failure', async () => {
    const api = fakeApi(
      {},
      {
        upsertMyInstallation: async () => {
          throw new Error('boom')
        },
        getPublicConfig: async () => {
          throw new Error('boom')
        },
      },
    )
    expect(await syncPeers(fakeDeps(api).deps)).toEqual({ outcome: 'ok' })
    expect(api.calls.filter((c) => c.startsWith('create:'))).toEqual(['create:wireguard'])
  })

  it('reports anything thrown as offline', async () => {
    const api = fakeApi(
      {},
      {
        getMyPeerByDevice: async () => {
          throw new TypeError('Failed to fetch')
        },
      },
    )
    expect(await syncPeers(fakeDeps(api).deps)).toEqual({ outcome: 'offline' })
  })
})

describe('applySyncResult', () => {
  it('writes the phase and the error', () => {
    const state: SetupState = emptySetupState()
    applySyncResult(state, { outcome: 'offline' })
    expect(state.phase).toBe('offline')

    applySyncResult(state, { outcome: 'error', errorKey: 'vpn.peerCreateFailed', detail: 'x' })
    expect(state).toEqual({ phase: 'idle', errorKey: 'vpn.peerCreateFailed', errorDetail: 'x' })

    applySyncResult(state, { outcome: 'ok' })
    expect(state.phase).toBe('idle')
  })
})

describe('createSyncSequencer', () => {
  beforeEach(() => vi.useFakeTimers())
  afterEach(() => vi.useRealTimers())

  function deferred<T>() {
    let resolve!: (v: T) => void
    const promise = new Promise<T>((r) => (resolve = r))
    return { promise, resolve }
  }

  it('applies a sync that finishes in time', async () => {
    const state = emptySetupState()
    state.errorKey = 'vpn.noSubscription'
    const { run } = createSyncSequencer(state, 1000)

    await run(Promise.resolve<SyncResult>({ outcome: 'ok' }))
    expect(state).toEqual(emptySetupState())
  })

  it('shows offline on timeout and applies a late verdict', async () => {
    const state = emptySetupState()
    const { run } = createSyncSequencer(state, 1000)
    const sync = deferred<SyncResult>()

    const running = run(sync.promise)
    await vi.advanceTimersByTimeAsync(1000)
    await running
    expect(state.phase).toBe('offline')

    sync.resolve({ outcome: 'error', errorKey: 'vpn.peerLimitReached' })
    await vi.advanceTimersByTimeAsync(0)
    expect(state.phase).toBe('idle')
    expect(state.errorKey).toBe('vpn.peerLimitReached')
  })

  it('keeps the banner when the late result is also offline', async () => {
    const state = emptySetupState()
    const { run } = createSyncSequencer(state, 1000)
    const sync = deferred<SyncResult>()

    const running = run(sync.promise)
    await vi.advanceTimersByTimeAsync(1000)
    await running
    sync.resolve({ outcome: 'offline' })
    await vi.advanceTimersByTimeAsync(0)
    expect(state.phase).toBe('offline')
  })

  it('lets only the newest run write', async () => {
    const state = emptySetupState()
    const { run } = createSyncSequencer(state, 1000)
    const first = deferred<SyncResult>()
    const second = deferred<SyncResult>()

    const firstRun = run(first.promise)
    const secondRun = run(second.promise)

    first.resolve({ outcome: 'error', errorKey: 'vpn.noSubscription' })
    await firstRun
    expect(state.errorKey).toBeNull()

    second.resolve({ outcome: 'ok' })
    await secondRun
    expect(state).toEqual(emptySetupState())
  })

  it('does not reapply a late result after the user already retried past offline', async () => {
    const state = emptySetupState()
    const { run } = createSyncSequencer(state, 1000)
    const slow = deferred<SyncResult>()

    const slowRun = run(slow.promise)
    await vi.advanceTimersByTimeAsync(1000)
    await slowRun
    expect(state.phase).toBe('offline')

    await run(Promise.resolve<SyncResult>({ outcome: 'ok' }))
    expect(state.phase).toBe('idle')

    slow.resolve({ outcome: 'error', errorKey: 'vpn.noSubscription' })
    await vi.advanceTimersByTimeAsync(0)
    expect(state.errorKey).toBeNull()
  })
})

describe('planOutcomeResponse', () => {
  const verifyFailed = (protocol: Protocol) => ({
    protocol,
    error: { kind: 'verify_failed' as const, detail: 'no handshake' },
    pass: 1,
  })
  const timedOut = {
    protocol: 'wireguard' as const,
    error: { kind: 'timed_out' as const },
    pass: 1,
  }

  it('ignores a cycle that connected, was cancelled, or went down', () => {
    expect(
      planOutcomeResponse({
        outcome: 'connected',
        protocol: 'wireguard',
        adopted: false,
        failures: [],
      }).action,
    ).toBe('ignore')
    expect(planOutcomeResponse({ outcome: 'cancelled' }).action).toBe('ignore')
    expect(planOutcomeResponse({ outcome: 'down' }).action).toBe('ignore')
  })

  it('repairs the peer of a protocol the ladder stepped over, without disturbing the tunnel', () => {
    // The device case: AmneziaWG failed to verify because its peer had been deleted, WireGuard
    // connected, and nothing repaired the dead peer until the next app start.
    expect(
      planOutcomeResponse({
        outcome: 'connected',
        protocol: 'wireguard',
        adopted: false,
        failures: [verifyFailed('amneziawg')],
      }),
    ).toEqual({ action: 'repair', protocol: 'amneziawg' })
  })

  it('has nothing to repair when the protocol that failed was VLESS', () => {
    // VLESS has no per-device peer: its config is per-user and a peer removal never touches it.
    expect(
      planOutcomeResponse({
        outcome: 'connected',
        protocol: 'wireguard',
        adopted: false,
        failures: [verifyFailed('vless')],
      }).action,
    ).toBe('ignore')
  })

  it('reports a failed unwind', () => {
    expect(planOutcomeResponse({ outcome: 'unwind_failed' })).toEqual({
      action: 'show_error',
      error: { kind: 'unwind_failed' },
    })
  })

  it('re-provisions the wg-family protocol whose verification failed, whatever was tried last', () => {
    expect(
      planOutcomeResponse({
        outcome: 'exhausted',
        failures: [verifyFailed('amneziawg'), timedOut],
      }),
    ).toEqual({ action: 'reprovision', protocol: 'amneziawg' })
    expect(
      planOutcomeResponse({ outcome: 'lost_gave_up', protocol: 'wireguard', passes: 3 }),
    ).toEqual({ action: 'reprovision', protocol: 'wireguard' })
  })

  it('treats a VLESS verification failure as a plain connection failure', () => {
    expect(
      planOutcomeResponse({ outcome: 'exhausted', failures: [verifyFailed('vless')] }),
    ).toEqual({ action: 'show_error', error: { kind: 'connection_failed' } })
    expect(planOutcomeResponse({ outcome: 'lost_gave_up', protocol: 'vless', passes: 1 })).toEqual({
      action: 'show_error',
      error: { kind: 'connection_failed' },
    })
  })

  it('shows the last probe error when no verification failed', () => {
    expect(planOutcomeResponse({ outcome: 'exhausted', failures: [timedOut] })).toEqual({
      action: 'show_error',
      error: { kind: 'attempt_failed', failure: timedOut },
    })
  })

  it('stays quiet when the last probe was cancelled, or there were none', () => {
    const cancelled = {
      protocol: 'wireguard' as const,
      error: { kind: 'cancelled' as const },
      pass: 1,
    }
    expect(
      planOutcomeResponse({ outcome: 'exhausted', failures: [timedOut, cancelled] }).action,
    ).toBe('ignore')
    expect(planOutcomeResponse({ outcome: 'exhausted', failures: [] }).action).toBe('ignore')
  })
})

describe('planWithoutReprovision', () => {
  it('shows a failure instead of looking the peer up again', () => {
    // The connect that follows a re-provisioning: the peer was just recreated, so a second
    // verification failure is not evidence that it is missing — and checking again would loop.
    expect(
      planWithoutReprovision({ outcome: 'lost_gave_up', protocol: 'wireguard', passes: 3 }),
    ).toEqual({ action: 'show_error', error: { kind: 'connection_failed' } })
    expect(
      planWithoutReprovision({
        outcome: 'exhausted',
        failures: [{ protocol: 'amneziawg', error: { kind: 'verify_failed' as const }, pass: 1 }],
      }),
    ).toEqual({ action: 'show_error', error: { kind: 'connection_failed' } })
  })

  it('leaves every other plan alone', () => {
    expect(planWithoutReprovision({ outcome: 'unwind_failed' })).toEqual({
      action: 'show_error',
      error: { kind: 'unwind_failed' },
    })
    expect(planWithoutReprovision({ outcome: 'cancelled' }).action).toBe('ignore')
  })
})

describe('reprovisionPeer', () => {
  function harness(found: 'yes' | 'no' | 'unknown', hasConfigAfter = true) {
    const calls: string[] = []
    const outcome = reprovisionPeer({
      lookup: async () => {
        calls.push('lookup')
        return found === 'yes' ? { found, id: 1 } : { found }
      },
      resync: async () => void calls.push('resync'),
      hasConfig: () => hasConfigAfter,
      reconnect: async () => void calls.push('reconnect'),
    })
    return { calls, outcome }
  }

  it('re-provisions exactly once and reconnects when the peer is gone', async () => {
    const { calls, outcome } = harness('no')
    expect(await outcome).toBe('reconnected')
    expect(calls).toEqual(['lookup', 'resync', 'reconnect'])
  })

  it('does not reconnect when re-provisioning produced no config', async () => {
    const { calls, outcome } = harness('no', false)
    expect(await outcome).toBe('no_config')
    expect(calls).toEqual(['lookup', 'resync'])
  })

  it('leaves an existing peer alone', async () => {
    const { calls, outcome } = harness('yes')
    expect(await outcome).toBe('peer_exists')
    expect(calls).toEqual(['lookup'])
  })

  it('reports an unreachable server without touching the peer', async () => {
    const { calls, outcome } = harness('unknown')
    expect(await outcome).toBe('unreachable')
    expect(calls).toEqual(['lookup'])
  })
})
