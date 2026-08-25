import { describe, expect, test } from 'vite-plus/test'

import {
  TRAFFIC_UNAVAILABLE,
  formatBytes,
  formatDuration,
  formatRelativeTime,
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

describe('formatRelativeTime', () => {
  const now = new Date('2026-08-25T12:00:00Z')
  const ago = (seconds: number) => new Date(now.getTime() - seconds * 1000)

  test('picks the coarsest unit that fits', () => {
    expect(formatRelativeTime(ago(5), 'en', now)).toBe('now')
    expect(formatRelativeTime(ago(90), 'en', now)).toBe('1 minute ago')
    expect(formatRelativeTime(ago(3 * 3600 + 5), 'en', now)).toBe('3 hours ago')
    expect(formatRelativeTime(ago(86400), 'en', now)).toBe('yesterday')
    expect(formatRelativeTime(ago(9 * 86400), 'en', now)).toBe('last week')
    expect(formatRelativeTime(ago(45 * 86400), 'en', now)).toBe('last month')
    expect(formatRelativeTime(ago(400 * 86400), 'en', now)).toBe('last year')
  })

  test('accepts ISO strings and the future, and follows the locale', () => {
    expect(formatRelativeTime(ago(2 * 3600).toISOString(), 'en', now)).toBe('2 hours ago')
    expect(formatRelativeTime(ago(-2 * 3600), 'en', now)).toBe('in 2 hours')
    expect(formatRelativeTime(ago(2 * 3600), 'ru', now)).toBe('2 часа назад')
  })
})
