import { describe, expect, test } from 'vite-plus/test'

import { toIntOrNull } from './number'

describe('toIntOrNull', () => {
  test('accepts whole numbers as numbers or strings', () => {
    expect(toIntOrNull(3)).toBe(3)
    expect(toIntOrNull('42')).toBe(42)
    expect(toIntOrNull(0)).toBe(0)
  })

  test('maps a cleared or unusable input to null', () => {
    expect(toIntOrNull('')).toBeNull()
    expect(toIntOrNull(null)).toBeNull()
    expect(toIntOrNull(undefined)).toBeNull()
    expect(toIntOrNull('1.5')).toBeNull()
    expect(toIntOrNull(Number.NaN)).toBeNull()
    expect(toIntOrNull('abc')).toBeNull()
  })
})
