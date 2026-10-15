import { beforeEach, describe, expect, it, vi } from 'vite-plus/test'
import { createPinia, setActivePinia } from 'pinia'
import type { Settings, SettingsState } from '../bindings'
import { useSettingsStore } from './settingsStore'

const { get, update } = vi.hoisted(() => ({ get: vi.fn(), update: vi.fn() }))
vi.mock('../bindings', () => ({
  commands: { tunnelGetSettings: get, tunnelUpdateSettings: update },
}))

function state(revision = 3): SettingsState {
  return {
    document: {
      schema_version: 1,
      revision,
      protocols: { mode: 'auto', auto_order: ['wireguard'], manual: 'wireguard' },
      routing: { allow_lan: false, excluded_routes: [], apps: { mode: 'all', items: [] } },
      recovery: { mode: 'limited', rounds: 7 },
      startup: { connect_on_boot: true },
    },
    intent_revision: null,
  }
}
beforeEach(() => {
  setActivePinia(createPinia())
  get.mockReset().mockResolvedValue({ status: 'ok', data: state() })
  update.mockReset().mockImplementation(async (revision: number, settings: Settings) => ({
    status: 'ok',
    data: {
      document: { schema_version: 1, revision: revision + 1, ...settings },
      intent_revision: null,
    },
  }))
})

describe('owner settings', () => {
  it('receives another client revision without a local write', async () => {
    const settings = useSettingsStore()
    await settings.loadOwnerSettings()
    const newer = state(4)
    newer.document.routing.allow_lan = true
    get.mockResolvedValue({ status: 'ok', data: newer })
    settings.observeRevision(4)
    await vi.waitFor(() => expect(settings.allowLan).toBe(true))
    expect(update).not.toHaveBeenCalled()
  })
  it('does not replace a pending draft when its own event precedes the write response', async () => {
    const settings = useSettingsStore()
    await settings.loadOwnerSettings()
    settings.allowLan = true
    settings.observeRevision(4)
    await settings.flushSettings()
    await Promise.resolve()
    expect(get).toHaveBeenCalledTimes(1)
    expect(settings.allowLan).toBe(true)
  })
  it('loads without writing defaults or expanding the protocol allowlist', async () => {
    const settings = useSettingsStore()
    await settings.loadOwnerSettings()
    expect(settings.protocolOrder).toEqual(['wireguard'])
    expect(update).not.toHaveBeenCalled()
  })
  it('serializes GUI changes with revisions and preserves fields not edited by the GUI', async () => {
    const settings = useSettingsStore()
    await settings.loadOwnerSettings()
    settings.allowLan = true
    settings.autoSelect = false
    await settings.flushSettings()
    expect(update.mock.calls.map((c) => c[0])).toEqual([3, 4])
    expect(update.mock.calls[1]?.[1]).toMatchObject({
      protocols: { mode: 'manual' },
      recovery: { mode: 'limited', rounds: 7 },
      startup: { connect_on_boot: true },
    })
    expect(update.mock.calls[0]?.[1]).not.toHaveProperty('revision')
  })
  it('reports a conflict and stops later writes until explicitly reloaded', async () => {
    update.mockResolvedValue({
      status: 'error',
      error: { kind: 'conflict', expected: 3, actual: 4 },
    })
    const settings = useSettingsStore()
    await settings.loadOwnerSettings()
    settings.allowLan = true
    settings.autoSelect = false
    await expect(settings.flushSettings()).rejects.toThrow('conflict')
    expect(update).toHaveBeenCalledTimes(1)
    await settings.reloadOwnerSettings()
    expect(settings.settingsError).toBeNull()
    expect(settings.allowLan).toBe(false)
  })
  it('never overwrites an unreadable owner document with local defaults', async () => {
    get.mockResolvedValue({
      status: 'error',
      error: { kind: 'failed', detail: 'invalid settings' },
    })
    const settings = useSettingsStore()
    await expect(settings.loadOwnerSettings()).rejects.toThrow('invalid settings')
    settings.allowLan = true
    expect(update).not.toHaveBeenCalled()
    expect(settings.settingsReady).toBe(false)
  })
})
