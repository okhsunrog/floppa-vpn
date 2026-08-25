import { describe, expect, it } from 'vite-plus/test'
import { isUnhandledOutcome, needsAttention } from './outcomes'
import type { CycleOutcome } from '../bindings'

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
