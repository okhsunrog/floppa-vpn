import { describe, expect, test } from 'vite-plus/test'

import {
  TRAFFIC_UNAVAILABLE,
  formatBytes,
  formatDuration,
  formatSpeed,
  formatSpeedLimit,
  formatTraffic,
} from './format'

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

  test('trimZeroSeconds drops only a zero seconds part', () => {
    expect(formatDuration(300, { trimZeroSeconds: true })).toBe('5m')
    expect(formatDuration(301, { trimZeroSeconds: true })).toBe('5m 1s')
    expect(formatDuration(300)).toBe('5m 0s')
    expect(formatDuration(45, { trimZeroSeconds: true })).toBe('45s')
  })
})

describe('formatSpeedLimit', () => {
  test('formats finite and unlimited limits', () => {
    expect(formatSpeedLimit(100)).toBe('100 Mbps')
    expect(formatSpeedLimit(null)).toBe('Unlimited')
    expect(formatSpeedLimit(undefined, 'Без ограничений')).toBe('Без ограничений')
  })
})

describe('formatTraffic', () => {
  test('renders the counter when the metrics are real', () => {
    expect(formatTraffic(2048, true)).toBe('2 KB')
  })

  test('renders a dash instead of a placeholder zero when they are not', () => {
    expect(formatTraffic(0, false)).toBe(TRAFFIC_UNAVAILABLE)
    expect(formatTraffic(2048, false)).toBe(TRAFFIC_UNAVAILABLE)
  })
})
