import { afterEach, beforeEach, describe, expect, it, vi } from 'vite-plus/test'
import {
  applySyncResult,
  createSyncSequencer,
  emptySetupState,
  planOutcomeResponse,
  type SetupState,
} from './usePeerProvisioning'
import type { Protocol, SyncOutcome } from '../bindings'

/*
 * What is left to test here is the card's own behaviour: how a result reaches the banner, how a
 * slow sync is sequenced against a faster one, and what a finished cycle is worth saying about.
 *
 * Provisioning itself is tested in `floppa-api-client`, against fakes, in the one place it now
 * lives.
 */

describe('applySyncResult', () => {
  it('writes the phase and the error', () => {
    const state: SetupState = emptySetupState()
    applySyncResult(state, { outcome: 'offline' })
    expect(state.phase).toBe('offline')

    applySyncResult(state, { outcome: 'failed', error: { kind: 'create_failed', detail: 'x' } })
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

    await run(Promise.resolve<SyncOutcome>({ outcome: 'ok' }))
    expect(state).toEqual(emptySetupState())
  })

  it('shows offline on timeout and applies a late verdict', async () => {
    const state = emptySetupState()
    const { run } = createSyncSequencer(state, 1000)
    const sync = deferred<SyncOutcome>()

    const running = run(sync.promise)
    await vi.advanceTimersByTimeAsync(1000)
    await running
    expect(state.phase).toBe('offline')

    sync.resolve({ outcome: 'failed', error: { kind: 'peer_limit_reached' } })
    await vi.advanceTimersByTimeAsync(0)
    expect(state.phase).toBe('idle')
    expect(state.errorKey).toBe('vpn.peerLimitReached')
  })

  it('keeps the banner when the late result is also offline', async () => {
    const state = emptySetupState()
    const { run } = createSyncSequencer(state, 1000)
    const sync = deferred<SyncOutcome>()

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
    const first = deferred<SyncOutcome>()
    const second = deferred<SyncOutcome>()

    const firstRun = run(first.promise)
    const secondRun = run(second.promise)

    first.resolve({ outcome: 'failed', error: { kind: 'no_subscription' } })
    await firstRun
    expect(state.errorKey).toBeNull()

    second.resolve({ outcome: 'ok' })
    await secondRun
    expect(state).toEqual(emptySetupState())
  })

  it('does not reapply a late result after the user already retried past offline', async () => {
    const state = emptySetupState()
    const { run } = createSyncSequencer(state, 1000)
    const slow = deferred<SyncOutcome>()

    const slowRun = run(slow.promise)
    await vi.advanceTimersByTimeAsync(1000)
    await slowRun
    expect(state.phase).toBe('offline')

    await run(Promise.resolve<SyncOutcome>({ outcome: 'ok' }))
    expect(state.phase).toBe('idle')

    slow.resolve({ outcome: 'failed', error: { kind: 'no_subscription' } })
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

  it('says nothing about a cycle that connected, whatever it stepped over on the way', () => {
    // AmneziaWG failed to verify because its peer had been deleted, WireGuard connected. The
    // dead peer is worth replacing and Rust does it, in the process the app being closed does
    // not freeze. There is nothing for the card to show about a tunnel that is up.
    expect(
      planOutcomeResponse({
        outcome: 'connected',
        protocol: 'wireguard',
        adopted: false,
        failures: [verifyFailed('amneziawg')],
      }).action,
    ).toBe('ignore')
  })

  it('reports a failed unwind', () => {
    expect(planOutcomeResponse({ outcome: 'unwind_failed' })).toEqual({
      action: 'show_error',
      error: { kind: 'unwind_failed' },
    })
  })

  it('shows the last probe error on an exhausted cycle, verification failure or not', () => {
    // A verification failure is shown like any other. Rust may be replacing the peer behind it,
    // and if that works the reconnect it asks for replaces this with a connected state.
    expect(
      planOutcomeResponse({
        outcome: 'exhausted',
        failures: [verifyFailed('amneziawg'), timedOut],
      }),
    ).toEqual({ action: 'show_error', error: { kind: 'attempt_failed', failure: timedOut } })
  })

  it('reports a tunnel it could not keep up', () => {
    expect(
      planOutcomeResponse({ outcome: 'lost_gave_up', protocol: 'wireguard', passes: 3 }),
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
