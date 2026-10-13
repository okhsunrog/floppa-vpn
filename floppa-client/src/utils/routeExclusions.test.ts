import { describe, expect, it } from 'vite-plus/test'
import { normalizeDestination } from './routeExclusions'
import { sameParams } from './splitRules'

describe('route exclusions', () => {
  it('normalizes subnet host bits and individual hosts', () => {
    expect(normalizeDestination(' 194.186.63.7/24 ')).toBe('194.186.63.0/24')
    expect(normalizeDestination('194.186.63.7')).toBe('194.186.63.7/32')
    expect(normalizeDestination('0.0.0.0/0')).toBe('0.0.0.0/0')
  })
  it('rejects invalid and unsupported addresses', () => {
    for (const raw of [
      '::/0',
      'example.com',
      '1.2.3.256',
      '1.2.3.4/33',
      '1.2.3.4/',
      '1.2.3.4/0x10',
    ]) {
      expect(normalizeDestination(raw)).toBeNull()
    }
  })
  it('detects added and removed exclusions, including old settings without the field', () => {
    const old = { split_mode: 'all' as const, apps: [] }
    const empty = { ...old, excluded_routes: [] }
    const excluded = { ...old, excluded_routes: ['194.186.63.0/24'] }
    expect(sameParams(old, empty)).toBe(true)
    expect(sameParams(old, excluded)).toBe(false)
    expect(sameParams(excluded, empty)).toBe(false)
    expect(sameParams(excluded, { ...excluded })).toBe(true)
  })
})
