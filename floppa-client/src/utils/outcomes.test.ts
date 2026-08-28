import { describe, expect, it } from 'vite-plus/test'
import { isUnhandledOutcome, needsAttention, planOutcomeResponse } from './outcomes'
import type { CycleOutcome, Protocol } from '../bindings'

const connected: CycleOutcome = {
  outcome: 'connected',
  protocol: 'amneziawg',
  adopted: false,
  failures: [],
}
const lostGaveUp: CycleOutcome = { outcome: 'lost_gave_up', protocol: 'amneziawg', passes: 3 }
void lostGaveUp

describe('isUnhandledOutcome', () => {
  it('treats everything as new before anything was handled', () => {
    expect(isUnhandledOutcome(null, 1)).toBe(true)
  })

  it('ignores the same outcome republished over and over', () => {
    // The actor republishes the whole state on every tick and the outcome stays put, so the same
    // serial arrives many times.
    expect(isUnhandledOutcome({ serial: 4, outcome: 'connected' }, 4)).toBe(false)
  })

  it('surfaces a second connected under the same intent', () => {
    // What a background reconnect looks like: the tunnel dropped, the ladder rebuilt it, and the
    // intent never changed — so epoch and tag are both identical to the connect the user asked
    // for. Only the serial tells them apart, and the second one is the one carrying "a protocol
    // was stepped over". Deduplicating by `{ epoch, outcome }` swallowed it, and a peer deleted
    // on the server went unrepaired on a real device because of it.
    expect(isUnhandledOutcome({ serial: 4, outcome: 'connected' }, 5)).toBe(true)
  })

  it('surfaces lost_gave_up after connected', () => {
    expect(isUnhandledOutcome({ serial: 4, outcome: 'connected' }, 5)).toBe(true)
  })
})

describe('needsAttention', () => {
  it('says nothing about the endings the user asked for', () => {
    expect(needsAttention(connected)).toBe(false)
    expect(needsAttention({ outcome: 'down' })).toBe(false)
    expect(needsAttention({ outcome: 'cancelled' })).toBe(false)
  })

  it('surfaces every ending without a tunnel', () => {
    expect(needsAttention(lostGaveUp)).toBe(true)
    expect(needsAttention({ outcome: 'exhausted', failures: [] })).toBe(true)
    expect(needsAttention({ outcome: 'unwind_failed' })).toBe(true)
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
