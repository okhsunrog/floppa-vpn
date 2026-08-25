import { describe, expect, test } from 'vite-plus/test'
import sharedLocaleEn from 'floppa-web-shared/locales/en'
import sharedLocaleRu from 'floppa-web-shared/locales/ru'
import {
  ATTEMPT_ERROR_KEYS,
  BACKEND_ERROR_KEYS,
  STEP_KEYS,
  VPN_ERROR_KEYS,
  describeVpnError,
} from './vpnErrors'

type Messages = { [key: string]: string | Messages }

/** Every locale that ships, so a key added to one and forgotten in the other is a failure. */
const LOCALES: Record<string, Messages> = {
  en: sharedLocaleEn as Messages,
  ru: sharedLocaleRu as Messages,
}

function lookupIn(messages: Messages, key: string): string | undefined {
  let node: string | Messages | undefined = messages
  for (const part of key.split('.')) {
    if (typeof node !== 'object' || node === undefined) return undefined
    node = node[part]
  }
  return typeof node === 'string' ? node : undefined
}

function lookup(key: string): string | undefined {
  return lookupIn(LOCALES.en, key)
}

/** A `t` that renders `key{params}` so a test can see both what was looked up and with what. */
const t = (key: string, params: Record<string, unknown> = {}) => {
  const message = lookup(key)
  if (message === undefined) throw new Error(`missing locale key ${key}`)
  return message.replace(/\{(\w+)\}/g, (_, name: string) => {
    const value = params[name]
    return typeof value === 'string' ? value : `{${name}}`
  })
}

const ALL_KEYS = [
  ...Object.values(VPN_ERROR_KEYS),
  ...Object.values(ATTEMPT_ERROR_KEYS),
  ...Object.values(BACKEND_ERROR_KEYS),
  ...Object.values(STEP_KEYS),
  'vpn.errors.attemptFailed',
]

describe('vpn error keys', () => {
  // Both locales, not just `en`: a missing `ru` key falls back to English at runtime, so nothing
  // breaks and nobody notices — which is exactly why it needs a test rather than a bug report.
  for (const [locale, messages] of Object.entries(LOCALES)) {
    test(`every kind resolves to a message in ${locale}`, () => {
      for (const key of ALL_KEYS) {
        expect(lookupIn(messages, key), `${locale}: ${key}`).toBeDefined()
      }
    })
  }

  test('a nested backend failure is worded all the way down', () => {
    const text = describeVpnError(
      {
        kind: 'attempt_failed',
        failure: {
          protocol: 'amneziawg',
          pass: 1,
          error: { kind: 'backend', error: { kind: 'engine', detail: 'boom' } },
        },
      },
      t,
    )
    expect(text).toContain('AmneziaWG')
    expect(text).toContain('boom')
    expect(text).not.toMatch(/\{\w+\}/)
  })

  test('a crashed attempt carries its detail', () => {
    const text = describeVpnError(
      {
        kind: 'attempt_failed',
        failure: { protocol: 'wireguard', pass: 1, error: { kind: 'crashed', detail: 'panic' } },
      },
      t,
    )
    expect(text).toContain('panic')
  })
})
