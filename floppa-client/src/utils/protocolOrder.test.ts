import { describe, expect, test } from 'vite-plus/test'
import { DEFAULT_PROTOCOL_ORDER, isProtocol, sanitizeProtocolOrder } from './protocolOrder'

describe('DEFAULT_PROTOCOL_ORDER', () => {
  test('leads with AmneziaWG and lists every protocol once', () => {
    expect(DEFAULT_PROTOCOL_ORDER[0]).toBe('amneziawg')
    expect(new Set(DEFAULT_PROTOCOL_ORDER).size).toBe(DEFAULT_PROTOCOL_ORDER.length)
  })
})

describe('isProtocol', () => {
  test('accepts only the generated protocol names', () => {
    expect(isProtocol('wireguard')).toBe(true)
    expect(isProtocol('openvpn')).toBe(false)
    expect(isProtocol(null)).toBe(false)
    expect(isProtocol(1)).toBe(false)
  })
})

describe('sanitizeProtocolOrder', () => {
  test('keeps a valid order as-is', () => {
    expect(sanitizeProtocolOrder(['vless', 'wireguard', 'amneziawg'])).toEqual([
      'vless',
      'wireguard',
      'amneziawg',
    ])
  })

  test('drops unknown entries and duplicates, appends missing protocols', () => {
    expect(sanitizeProtocolOrder(['wireguard', 'openvpn', 'wireguard'])).toEqual([
      'wireguard',
      'amneziawg',
      'vless',
    ])
  })

  test('falls back to the default for anything that is not an array', () => {
    expect(sanitizeProtocolOrder(undefined)).toEqual([...DEFAULT_PROTOCOL_ORDER])
    expect(sanitizeProtocolOrder('wireguard')).toEqual([...DEFAULT_PROTOCOL_ORDER])
  })
})
