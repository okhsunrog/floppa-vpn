import { ref } from 'vue'
import { commands, type LogCaptureStatus, type LogConfig, type LogProfile } from '../bindings'
import { describeUnknown } from '../utils/errors'

/**
 * `LogConfig` with every field present.
 *
 * The generated type has them all optional, because the Rust struct carries `#[serde(default)]`
 * so a partial `log-config.json` from an older build still parses. Nothing ever *sends* a partial
 * one, so this narrows what we hold. Derived from the generated type rather than restated: a
 * hand-written copy stood here, and a field added in Rust would never have reached it.
 */
export type ResolvedLogConfig = { [K in keyof LogConfig]-?: LogConfig[K] }

export function resolveLogConfig(cfg: LogConfig): ResolvedLogConfig {
  return {
    profile: cfg.profile ?? 'normal',
    custom_filter: cfg.custom_filter ?? null,
    custom_filter_enabled: cfg.custom_filter_enabled ?? false,
  }
}

/** The Tauri commands the diagnostics card drives — a slice of `commands`, so tests can fake it. */
export type LogCommands = Pick<
  typeof commands,
  | 'getLogConfig'
  | 'setLogConfig'
  | 'getLogCaptureStatus'
  | 'startLogCapture'
  | 'stopLogCapture'
  | 'exportLogs'
>

/**
 * What the user should be told about. Typed rather than toasted from here, so the composable
 * knows nothing about i18n or the toast API and the card decides the wording.
 */
export type LogNotice =
  | { kind: 'load_failed'; detail: string }
  | { kind: 'save_failed' }
  | { kind: 'capture_failed'; detail: string }
  | { kind: 'export_failed' }
  | { kind: 'exported' }

/**
 * Runtime logging settings and capture control.
 *
 * Holds the resolved config, the capture status and the text of the custom-filter input, which
 * is deliberately separate from the stored filter: typing is not applying.
 */
export function useLogConfig(notify: (n: LogNotice) => void, api: LogCommands = commands) {
  const logConfig = ref<ResolvedLogConfig>({
    profile: 'normal',
    custom_filter: null,
    custom_filter_enabled: false,
  })
  const captureStatus = ref<LogCaptureStatus>({ active: false, capture_id: null })
  const customFilterInput = ref('')

  const saving = ref(false)
  const captureBusy = ref(false)
  const exporting = ref(false)

  async function loadConfig() {
    logConfig.value = resolveLogConfig(await api.getLogConfig())
    customFilterInput.value = logConfig.value.custom_filter ?? ''
  }

  async function loadCaptureStatus() {
    captureStatus.value = await api.getLogCaptureStatus()
  }

  /**
   * Both commands are infallible on the Rust side, so only a broken IPC gets here — still worth
   * a notice rather than an unhandled rejection with the diagnostics card left blank.
   */
  async function load() {
    try {
      await loadConfig()
      await loadCaptureStatus()
    } catch (e) {
      console.error('Failed to load the log settings:', e)
      notify({ kind: 'load_failed', detail: describeUnknown(e) })
    }
  }

  async function save() {
    saving.value = true
    try {
      const result = await api.setLogConfig(logConfig.value)
      if (result.status === 'error') {
        console.error('Failed to save log config:', result.error)
        notify({ kind: 'save_failed' })
      }
    } finally {
      saving.value = false
    }
  }

  async function setProfile(profile: LogProfile) {
    logConfig.value.profile = profile
    await save()
  }

  /** Store the typed filter and switch it on; an empty input clears it instead. */
  async function applyCustomFilter() {
    logConfig.value.custom_filter = customFilterInput.value || null
    logConfig.value.custom_filter_enabled = Boolean(logConfig.value.custom_filter)
    await save()
  }

  async function setCustomFilterEnabled(enabled: boolean) {
    logConfig.value.custom_filter_enabled = enabled
    await save()
  }

  async function clearCustomFilter() {
    customFilterInput.value = ''
    logConfig.value.custom_filter = null
    logConfig.value.custom_filter_enabled = false
    await save()
  }

  /** Start a capture, or stop the running one. Stopping reloads the config the capture pinned. */
  async function toggleCapture() {
    captureBusy.value = true
    try {
      const result = captureStatus.value.active
        ? await api.stopLogCapture()
        : await api.startLogCapture()
      if (result.status === 'error') {
        notify({ kind: 'capture_failed', detail: result.error })
        return
      }
      captureStatus.value = result.data
      await loadConfig()
    } finally {
      captureBusy.value = false
    }
  }

  async function exportLogs() {
    exporting.value = true
    try {
      const result = await api.exportLogs()
      if (result.status === 'error') {
        notify({ kind: 'export_failed' })
        return
      }
      if (result.data) notify({ kind: 'exported' })
    } catch (e) {
      console.error('Failed to export logs:', e)
      notify({ kind: 'export_failed' })
    } finally {
      exporting.value = false
    }
  }

  return {
    logConfig,
    captureStatus,
    customFilterInput,
    saving,
    captureBusy,
    exporting,
    load,
    setProfile,
    applyCustomFilter,
    setCustomFilterEnabled,
    clearCustomFilter,
    toggleCapture,
    exportLogs,
  }
}
