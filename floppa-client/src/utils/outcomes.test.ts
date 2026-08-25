import { describe, expect, it } from 'vite-plus/test'
import { isUnhandledOutcome, needsAttention } from './outcomes'
import type { CycleOutcome } from '../bindings'

const connected: CycleOutcome = { outcome: 'connected', protocol: 'amneziawg', adopted: false }
const lostGaveUp: CycleOutcome = { outcome: 'lost_gave_up', protocol: 'amneziawg', passes: 3 }

describe('isUnhandledOutcome', () => {
  it('treats everything as new before anything was handled', () => {
    expect(isUnhandledOutcome(null, 1, connected)).toBe(true)
  })

  it('ignores the same outcome republished for the same epoch', () => {
    expect(isUnhandledOutcome({ epoch: 1, outcome: 'connected', seq: 4 }, 1, connected)).toBe(false)
  })

  it('surfaces lost_gave_up after connected within one epoch', () => {
    expect(isUnhandledOutcome({ epoch: 1, outcome: 'connected', seq: 4 }, 1, lostGaveUp)).toBe(true)
  })

  it('treats a new epoch as new even with the same outcome', () => {
    expect(isUnhandledOutcome({ epoch: 1, outcome: 'lost_gave_up', seq: 4 }, 2, lostGaveUp)).toBe(
      true,
    )
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
