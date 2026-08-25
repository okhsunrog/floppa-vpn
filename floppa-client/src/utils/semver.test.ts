import { describe, expect, test } from 'vite-plus/test'
import { compareSemver, parseSemver } from './semver'

describe('parseSemver', () => {
  test('reads the release triple', () => {
    expect(parseSemver('1.2.3')).toEqual([1, 2, 3])
  })

  test('ignores prerelease and build suffixes and leading whitespace', () => {
    expect(parseSemver(' 1.2.3-beta.1+build.7')).toEqual([1, 2, 3])
  })

  test('rejects anything that is not a triple', () => {
    expect(parseSemver('1.2')).toBeNull()
    expect(parseSemver('v1.2.3')).toBeNull()
    expect(parseSemver('')).toBeNull()
    expect(parseSemver('latest')).toBeNull()
  })
})

describe('compareSemver', () => {
  test('orders by major, then minor, then patch', () => {
    expect(compareSemver('2.0.0', '1.9.9')).toBeGreaterThan(0)
    expect(compareSemver('1.3.0', '1.2.9')).toBeGreaterThan(0)
    expect(compareSemver('1.2.3', '1.2.4')).toBeLessThan(0)
    expect(compareSemver('1.2.3', '1.2.3')).toBe(0)
  })

  test('compares numerically, not lexically', () => {
    expect(compareSemver('1.10.0', '1.9.0')).toBeGreaterThan(0)
  })

  test('is null when either side is unreadable, never "newer"', () => {
    expect(compareSemver('garbage', '1.0.0')).toBeNull()
    expect(compareSemver('1.0.0', '')).toBeNull()
  })
})
