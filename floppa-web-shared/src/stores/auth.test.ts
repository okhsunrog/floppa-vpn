import { describe, expect, test } from 'vite-plus/test'

import { isTokenExpired, jwtExp } from './auth'

/** An unsigned JWT-shaped token: `header.payload.signature`, base64url like the real thing. */
function fakeJwt(payload: unknown): string {
  const b64url = (s: string) => btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
  return `${b64url('{"alg":"HS256"}')}.${b64url(JSON.stringify(payload))}.sig`
}

describe('jwtExp', () => {
  test('reads a numeric exp claim through base64url', () => {
    // A payload long enough to contain '-' / '_' once base64url-encoded.
    expect(jwtExp(fakeJwt({ sub: '~~~???', exp: 1_800_000_000 }))).toBe(1_800_000_000)
  })

  test('is null without an exp claim or a parseable payload', () => {
    expect(jwtExp(fakeJwt({ sub: 'x' }))).toBeNull()
    expect(jwtExp(fakeJwt({ exp: '123' }))).toBeNull()
    expect(jwtExp('not-a-jwt')).toBeNull()
    expect(jwtExp('a.%%%.c')).toBeNull()
  })
})

describe('isTokenExpired', () => {
  test('is true only for a past exp', () => {
    const now = Math.floor(Date.now() / 1000)
    expect(isTokenExpired(fakeJwt({ exp: now - 60 }))).toBe(true)
    expect(isTokenExpired(fakeJwt({ exp: now + 3600 }))).toBe(false)
  })

  test('treats a token without exp as not expired', () => {
    expect(isTokenExpired(fakeJwt({ sub: 'x' }))).toBe(false)
    expect(isTokenExpired('garbage')).toBe(false)
  })
})
