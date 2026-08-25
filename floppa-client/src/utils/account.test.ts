import { describe, expect, it } from 'vite-plus/test'
import { accountChange } from './account'

describe('accountChange', () => {
  it('does nothing on the first sign-in of a session', () => {
    expect(accountChange(undefined, 7)).toBe('none')
    expect(accountChange(null, 7)).toBe('none')
  })

  it('does nothing while the same account stays signed in', () => {
    expect(accountChange(7, 7)).toBe('none')
  })

  it('forgets what the device holds when the account signs out', () => {
    expect(accountChange(7, null)).toBe('forget')
  })

  it('forgets and re-asks when a different account signs in', () => {
    expect(accountChange(7, 8)).toBe('switch')
  })
})
