import { describe, expect, test } from 'vite-plus/test'

import { sessionIcon, sessionTitle, useSessionConfirm } from './sessions'

describe('sessionTitle', () => {
  test('prefers the stamped label, then the device, then the platform', () => {
    expect(sessionTitle({ label: 'android · Pixel 7', device_name: 'Pixel 7' }, '?')).toBe(
      'android · Pixel 7',
    )
    expect(sessionTitle({ label: null, device_name: 'Pixel 7', platform: 'android' }, '?')).toBe(
      'Pixel 7',
    )
    expect(sessionTitle({ platform: 'linux' }, '?')).toBe('linux')
    expect(sessionTitle({ label: '', device_name: null, platform: undefined }, 'unnamed')).toBe(
      'unnamed',
    )
  })
})

describe('sessionIcon', () => {
  test('follows the platform, then the login kind', () => {
    expect(sessionIcon({ platform: 'Android', kind: 'deep_link' })).toBe('i-lucide-smartphone')
    expect(sessionIcon({ platform: 'linux', kind: 'deep_link' })).toBe('i-lucide-laptop')
    expect(sessionIcon({ platform: null, kind: 'telegram_widget' })).toBe('i-lucide-globe')
    expect(sessionIcon({ kind: 'credential' })).toBe('i-lucide-monitor-smartphone')
  })
})

describe('useSessionConfirm', () => {
  test('runs the action for the pending id, then closes', async () => {
    const { open, pendingId, request, confirm } = useSessionConfirm()
    const seen: string[] = []
    await confirm(async (id) => void seen.push(id))
    expect(seen).toEqual([])

    request('abc')
    expect(open.value).toBe(true)
    expect(pendingId.value).toBe('abc')
    await confirm(async (id) => void seen.push(id))
    expect(seen).toEqual(['abc'])
    expect(open.value).toBe(false)
    expect(pendingId.value).toBeNull()
  })
})
