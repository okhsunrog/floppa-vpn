import { describe, expect, it } from 'vite-plus/test'
import type { LogCaptureStatus, LogConfig } from '../bindings'
import { resolveLogConfig, useLogConfig, type LogCommands, type LogNotice } from './useLogConfig'

/** A Rust side holding one config and one capture flag, with every call recorded. */
function fakeCommands(
  initial: LogConfig = {},
  overrides: Partial<LogCommands> = {},
): LogCommands & { stored: LogConfig; calls: string[] } {
  let capture: LogCaptureStatus = { active: false, capture_id: null }
  const state = {
    stored: initial,
    calls: [] as string[],
  }
  return {
    ...state,
    async getLogConfig() {
      state.calls.push('get')
      return this.stored
    },
    async setLogConfig(config: LogConfig) {
      state.calls.push('set')
      this.stored = { ...config }
      return { status: 'ok', data: null }
    },
    async getLogCaptureStatus() {
      state.calls.push('status')
      return capture
    },
    async startLogCapture() {
      state.calls.push('start')
      capture = { active: true, capture_id: 'cap-1' }
      return { status: 'ok', data: capture }
    },
    async stopLogCapture() {
      state.calls.push('stop')
      capture = { active: false, capture_id: 'cap-1' }
      return { status: 'ok', data: capture }
    },
    async exportLogs() {
      state.calls.push('export')
      return { status: 'ok', data: true }
    },
    ...overrides,
  }
}

function harness(initial?: LogConfig, overrides?: Partial<LogCommands>) {
  const notices: LogNotice[] = []
  const api = fakeCommands(initial, overrides)
  const log = useLogConfig((n) => notices.push(n), api)
  return { api, notices, log }
}

describe('resolveLogConfig', () => {
  it('fills in the defaults a partial config from an older build leaves out', () => {
    expect(resolveLogConfig({})).toEqual({
      profile: 'normal',
      custom_filter: null,
      custom_filter_enabled: false,
    })
    expect(resolveLogConfig({ profile: 'verbose', custom_filter: 'x' })).toEqual({
      profile: 'verbose',
      custom_filter: 'x',
      custom_filter_enabled: false,
    })
  })
})

describe('useLogConfig', () => {
  it('loads the config and the capture status, seeding the filter input', async () => {
    const { log, api } = harness({ profile: 'verbose', custom_filter: 'debug' })
    await log.load()
    expect(log.logConfig.value).toEqual({
      profile: 'verbose',
      custom_filter: 'debug',
      custom_filter_enabled: false,
    })
    expect(log.customFilterInput.value).toBe('debug')
    expect(log.captureStatus.value).toEqual({ active: false, capture_id: null })
    expect(api.calls).toEqual(['get', 'status'])
  })

  it('reports a broken load once and leaves the defaults in place', async () => {
    const { log, notices } = harness(
      {},
      {
        getLogConfig: async () => {
          throw new Error('ipc down')
        },
      },
    )
    await log.load()
    expect(notices).toEqual([{ kind: 'load_failed', detail: 'ipc down' }])
    expect(log.logConfig.value.profile).toBe('normal')
  })

  it('saves a profile change and clears the saving flag afterwards', async () => {
    const { log, api } = harness()
    const saving = log.setProfile('verbose')
    expect(log.saving.value).toBe(true)
    await saving
    expect(log.saving.value).toBe(false)
    expect(api.stored).toEqual({
      profile: 'verbose',
      custom_filter: null,
      custom_filter_enabled: false,
    })
  })

  it('applying a typed filter stores and enables it; applying nothing clears it', async () => {
    const { log, api } = harness()
    log.customFilterInput.value = 'floppa=trace'
    await log.applyCustomFilter()
    expect(log.logConfig.value).toMatchObject({
      custom_filter: 'floppa=trace',
      custom_filter_enabled: true,
    })

    log.customFilterInput.value = ''
    await log.applyCustomFilter()
    expect(api.stored).toMatchObject({ custom_filter: null, custom_filter_enabled: false })
  })

  it('toggles and clears the filter without touching the profile', async () => {
    const { log, api } = harness({ profile: 'verbose', custom_filter: 'x' })
    await log.load()

    await log.setCustomFilterEnabled(true)
    expect(api.stored).toMatchObject({ profile: 'verbose', custom_filter_enabled: true })

    await log.clearCustomFilter()
    expect(log.customFilterInput.value).toBe('')
    expect(api.stored).toEqual({
      profile: 'verbose',
      custom_filter: null,
      custom_filter_enabled: false,
    })
  })

  it('reports a refused save', async () => {
    const { log, notices } = harness(
      {},
      { setLogConfig: async () => ({ status: 'error', error: 'read-only fs' }) },
    )
    await log.setProfile('verbose')
    expect(notices).toEqual([{ kind: 'save_failed' }])
  })

  it('starts, then stops a capture, re-reading the config each time', async () => {
    const { log, api } = harness()
    await log.toggleCapture()
    expect(log.captureStatus.value).toEqual({ active: true, capture_id: 'cap-1' })
    expect(log.captureBusy.value).toBe(false)

    await log.toggleCapture()
    expect(log.captureStatus.value).toEqual({ active: false, capture_id: 'cap-1' })
    expect(api.calls).toEqual(['start', 'get', 'stop', 'get'])
  })

  it('surfaces a capture failure with its detail and keeps the old status', async () => {
    const { log, notices } = harness(
      {},
      { startLogCapture: async () => ({ status: 'error', error: 'no space' }) },
    )
    await log.toggleCapture()
    expect(notices).toEqual([{ kind: 'capture_failed', detail: 'no space' }])
    expect(log.captureStatus.value.active).toBe(false)
  })

  it('announces a completed export, and only then', async () => {
    const exported = harness()
    await exported.log.exportLogs()
    expect(exported.notices).toEqual([{ kind: 'exported' }])

    // The user closed the share sheet: nothing to say
    const dismissed = harness({}, { exportLogs: async () => ({ status: 'ok', data: false }) })
    await dismissed.log.exportLogs()
    expect(dismissed.notices).toEqual([])

    const refused = harness({}, { exportLogs: async () => ({ status: 'error', error: 'x' }) })
    await refused.log.exportLogs()
    expect(refused.notices).toEqual([{ kind: 'export_failed' }])

    const thrown = harness(
      {},
      {
        exportLogs: async () => {
          throw new Error('ipc')
        },
      },
    )
    await thrown.log.exportLogs()
    expect(thrown.notices).toEqual([{ kind: 'export_failed' }])
    expect(thrown.log.exporting.value).toBe(false)
  })
})
