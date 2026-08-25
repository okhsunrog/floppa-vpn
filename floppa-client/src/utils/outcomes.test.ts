import { describe, expect, it } from 'vite-plus/test'
import { isUnhandledOutcome } from './outcomes'
import type { CycleOutcome } from '../bindings'

const connected: CycleOutcome = { outcome: 'connected', protocol: 'amneziawg', adopted: false }
const lostGaveUp: CycleOutcome = { outcome: 'lost_gave_up', protocol: 'amneziawg', passes: 3 }

describe('isUnhandledOutcome', () => {
  it('treats everything as new before anything was handled', () => {
    expect(isUnhandledOutcome(null, 1, connected)).toBe(true)
  })

  it('ignores the same outcome republished for the same epoch', () => {
    expect(isUnhandledOutcome({ epoch: 1, outcome: 'connected' }, 1, connected)).toBe(false)
  })

  it('surfaces lost_gave_up after connected within one epoch', () => {
    expect(isUnhandledOutcome({ epoch: 1, outcome: 'connected' }, 1, lostGaveUp)).toBe(true)
  })

  it('treats a new epoch as new even with the same outcome', () => {
    expect(isUnhandledOutcome({ epoch: 1, outcome: 'lost_gave_up' }, 2, lostGaveUp)).toBe(true)
  })
})
