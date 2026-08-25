import { describe, expect, test } from 'vite-plus/test'
import { extractDeepLinkLoginCode } from './deepLink'

describe('extractDeepLinkLoginCode', () => {
  test('reads the code from the host form', () => {
    expect(extractDeepLinkLoginCode('floppa://auth?code=abc123')).toBe('abc123')
  })

  test('reads the code from the path form', () => {
    expect(extractDeepLinkLoginCode('floppa:///auth?code=abc123')).toBe('abc123')
  })

  test('is null without a code', () => {
    expect(extractDeepLinkLoginCode('floppa://auth')).toBeNull()
  })

  test('ignores other schemes and other routes', () => {
    expect(extractDeepLinkLoginCode('https://auth?code=abc')).toBeNull()
    expect(extractDeepLinkLoginCode('floppa://settings?code=abc')).toBeNull()
  })

  test('is null for something that is not a URL', () => {
    expect(extractDeepLinkLoginCode('not a url')).toBeNull()
    expect(extractDeepLinkLoginCode('')).toBeNull()
  })
})
