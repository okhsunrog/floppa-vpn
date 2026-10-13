import { describe, expect, it } from 'vite-plus/test'
import { normalizeDestination, sortDestinationRules } from './destinationRules'

describe('destination routing', () => {
  it('normalizes hosts, subnet host bits and the catch-all', () => {
    expect(normalizeDestination(' 194.186.63.9/24 ')).toBe('194.186.63.0/24')
    expect(normalizeDestination('160.79.104.1')).toBe('160.79.104.1/32')
    expect(normalizeDestination('192.168.1.2/0')).toBe('0.0.0.0/0')
  })
  it('rejects unsupported addresses and malformed prefixes', () => {
    for (const input of [
      '::/0',
      'example.com',
      '1.2.3.256',
      '1.2.3.4/',
      '1.2.3.4/33',
      '1.2.3.4/0x10',
      '1.2.3.4/ 1',
      '1.2.3.4/2/3',
    ]) {
      expect(normalizeDestination(input)).toBeNull()
    }
  })
  it('puts exceptions before broader rules without mutating the input', () => {
    const rules = ['0.0.0.0/0', '10.0.0.0/8', '10.1.0.0/16'].map((destination) => ({
      name: '',
      destination,
      target: 'direct' as const,
    }))
    expect(sortDestinationRules(rules).map((r) => r.destination)).toEqual([
      '10.1.0.0/16',
      '10.0.0.0/8',
      '0.0.0.0/0',
    ])
    expect(rules[0]?.destination).toBe('0.0.0.0/0')
  })
})
