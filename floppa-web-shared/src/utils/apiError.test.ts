import { describe, expect, test } from 'vite-plus/test'

import { describeError, isApiError } from './apiError'

describe('isApiError', () => {
  test('recognises the server error body', () => {
    expect(isApiError({ error: 'conflict', message: 'Plan has existing subscriptions' })).toBe(true)
  })

  test('rejects everything the client throws that is not a parsed body', () => {
    expect(isApiError(new TypeError('Failed to fetch'))).toBe(false)
    expect(isApiError('<html>502 Bad Gateway</html>')).toBe(false)
    expect(isApiError(null)).toBe(false)
    expect(isApiError(undefined)).toBe(false)
    expect(isApiError({ error: 'conflict' })).toBe(false)
    expect(isApiError({ status: 409 })).toBe(false)
  })
})

describe('describeError', () => {
  test('prefers the server message for API errors', () => {
    expect(describeError({ error: 'peer_limit_reached', message: 'Peer limit: 2/2' }, 'x')).toBe(
      'Peer limit: 2/2',
    )
  })

  test('falls back for network errors and unparseable bodies', () => {
    expect(describeError(new TypeError('Failed to fetch'), 'Server unavailable')).toBe(
      'Server unavailable',
    )
    expect(describeError('gateway timeout', 'fallback')).toBe('fallback')
  })
})
