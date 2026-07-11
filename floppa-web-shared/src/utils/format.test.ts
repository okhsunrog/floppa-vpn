import { describe, expect, test } from 'vite-plus/test'

import { formatBytes, formatDuration, formatSpeed, formatSpeedLimit } from './format'

describe('formatBytes', () => {
  test('formats byte quantities and clamps units', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(1536)).toBe('1.5 KB')
    expect(formatBytes(1024 ** 6)).toBe('1024 PB')
  })
})

describe('formatSpeed', () => {
  test('formats transfer rates and clamps units', () => {
    expect(formatSpeed(0)).toBe('0 B/s')
    expect(formatSpeed(1536)).toBe('1.5 KB/s')
    expect(formatSpeed(1024 ** 4)).toBe('1024 GB/s')
  })
})

describe('formatDuration', () => {
  test.each([
    [59, '59s'],
    [90, '1m 30s'],
    [3_661, '1h 1m'],
    [90_000, '1d 1h'],
  ])('formats %i seconds as %s', (seconds, expected) => {
    expect(formatDuration(seconds)).toBe(expected)
  })
})

describe('formatSpeedLimit', () => {
  test('formats finite and unlimited limits', () => {
    expect(formatSpeedLimit(100)).toBe('100 Mbps')
    expect(formatSpeedLimit(null)).toBe('Unlimited')
    expect(formatSpeedLimit(undefined, 'Без ограничений')).toBe('Без ограничений')
  })
})
