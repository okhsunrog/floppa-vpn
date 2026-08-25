import { describe, expect, test } from 'vite-plus/test'

import en from './en'
import ru from './ru'

type Messages = { [key: string]: string | Messages }

function flatten(messages: Messages, prefix = ''): Map<string, string> {
  const out = new Map<string, string>()
  for (const [key, value] of Object.entries(messages)) {
    const path = prefix + key
    if (typeof value === 'string') out.set(path, value)
    else for (const [k, v] of flatten(value, path + '.')) out.set(k, v)
  }
  return out
}

const enKeys = flatten(en)
const ruKeys = flatten(ru)

/**
 * Named interpolation slots (`{name}`) a message uses, as a sorted set — a set because plural
 * forms (`a | b | c`) repeat the same slot a language-dependent number of times.
 */
function placeholders(message: string): string[] {
  return [...new Set([...message.matchAll(/\{(\w+)\}/g)].map((m) => m[1] ?? ''))].sort()
}

describe('locales', () => {
  test('en and ru define exactly the same keys', () => {
    expect([...ruKeys.keys()].sort()).toEqual([...enKeys.keys()].sort())
  })

  test('no message is empty', () => {
    for (const [key, value] of [...enKeys, ...ruKeys]) {
      expect(value.trim(), key).not.toBe('')
    }
  })

  test('translations use the same interpolation slots', () => {
    for (const [key, enMessage] of enKeys) {
      const ruMessage = ruKeys.get(key)
      if (ruMessage === undefined) continue // reported by the key test above
      expect(placeholders(ruMessage), key).toEqual(placeholders(enMessage))
    }
  })
})
