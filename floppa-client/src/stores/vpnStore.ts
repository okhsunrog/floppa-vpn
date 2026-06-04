import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import {
  commands,
  type ConnectionInfo,
  type ConfigSafe,
  type ConnectError,
  type Protocol,
} from '../bindings'
import { useSettingsStore } from './settingsStore'
import { platform } from '@tauri-apps/plugin-os'
import { i18n } from '../i18n'

const t = i18n.global.t

const MAX_RECONNECT_ATTEMPTS = 3

/** Result of a single connect attempt — success, or the typed failure category. */
type ConnectOutcome = { ok: true } | { ok: false; error: ConnectError }

export const useVpnStore = defineStore(
  'vpn',
  () => {
    const config = ref<ConfigSafe | null>(null)
    const connectionInfo = ref<ConnectionInfo | null>(null)
    const isLoading = ref(false)
    const error = ref<string | null>(null)
    // Structured error from the last connect() attempt — lets callers branch on
    // the failure category (e.g. re-provision peer on 'verify_failed') instead of
    // string-matching the human-readable message.
    const connectError = ref<ConnectError | null>(null)
    const isAndroid = ref(false)
    const deviceId = ref<string | null>(null)
    const deviceName = ref<string | null>(null)
    const availableProtocols = ref<Protocol[]>([])

    // What the user wants the tunnel to be. Auto-reconnect only fires while the
    // user intends to stay connected; an explicit disconnect flips this to
    // 'disconnected' so a clean teardown isn't mistaken for a dropped tunnel.
    const userIntent = ref<'connected' | 'disconnected'>('disconnected')

    // Auto-reconnect state
    const reconnectAttempts = ref(0)
    let reconnectTimeoutId: ReturnType<typeof setTimeout> | null = null
    let onReconnectFailed: (() => void) | null = null

    // Auto-protocol-select state. `attempt` describes the protocol currently being
    // probed (for the UI stepper); `abortGen` is bumped on disconnect so an
    // in-flight probe cycle bails instead of fighting a user-requested teardown.
    const attempt = ref<{ protocol: Protocol; index: number; total: number } | null>(null)
    let abortGen = 0

    // Re-entrancy guard for connect(): true for the whole duration of a connect
    // cycle. The auto-reconnect timer, a user tap, and the status poll can each
    // call connect() concurrently; without this each would spin its own probe +
    // poll loop and race on shared state. Intentionally NOT isLoading — the UI also
    // toggles isLoading around the verify-failed peer-recreation flow, so reusing it
    // here would wrongly block that legitimate second connect().
    let connectInFlight = false

    const isConnected = computed(() => connectionInfo.value?.status === 'connected')
    const hasConfig = computed(() => config.value !== null)
    const activeProtocol = computed(() => config.value?.protocol ?? 'wireguard')

    const connectionStatus = computed(() => {
      if (isConnected.value) return 'connected' as const
      const s = connectionInfo.value?.status
      if (s === 'connecting' || s === 'verifying_connection') return 'connecting' as const
      if (s === 'disconnecting') return 'disconnecting' as const
      return 'disconnected' as const
    })

    // Initialize platform detection and device info
    async function initPlatform() {
      try {
        const os = platform()
        isAndroid.value = os === 'android'

        // Load device identifiers
        try {
          const result = await commands.getDeviceId()
          if (result.status === 'ok') deviceId.value = result.data
        } catch (e) {
          console.error('Failed to get device id:', e)
        }
        try {
          deviceName.value = await commands.getDeviceName()
        } catch (e) {
          console.error('Failed to get device name:', e)
        }
      } catch (e) {
        console.error('[vpnStore] Failed to detect platform:', e)
      }
    }

    function cleanup() {
      if (reconnectTimeoutId) {
        clearTimeout(reconnectTimeoutId)
        reconnectTimeoutId = null
      }
    }

    /** Set config from server API response and persist it */
    async function setActiveConfig(configStr: string) {
      error.value = null
      try {
        const result = await commands.setActiveConfig(configStr)
        if (result.status === 'error') {
          error.value = result.error
          return
        }
        await loadConfig()
      } catch (e) {
        error.value = String(e)
      }
    }

    /** Clear config from memory and delete persisted config */
    async function clearConfig() {
      isLoading.value = true
      error.value = null
      userIntent.value = 'disconnected'
      abortGen++
      attempt.value = null
      reconnectAttempts.value = 0
      if (reconnectTimeoutId) {
        clearTimeout(reconnectTimeoutId)
        reconnectTimeoutId = null
      }
      try {
        const result = await commands.clearConfig()
        if (result.status === 'error') {
          error.value = result.error
          return
        }
        config.value = null
        connectionInfo.value = null
      } catch (e) {
        error.value = String(e)
      } finally {
        isLoading.value = false
      }
    }

    async function loadConfig() {
      try {
        // Try loading persisted config from keyring/file into memory
        await commands.loadSavedConfig()
        const result = await commands.getConfig()
        if (result.status === 'ok') {
          config.value = result.data
        } else {
          console.error('Failed to load config:', result.error)
        }
        // Load available protocols
        const protocolsResult = await commands.getAvailableProtocols()
        if (protocolsResult.status === 'ok') {
          availableProtocols.value = protocolsResult.data
        }
      } catch (e) {
        console.error('Failed to load config:', e)
      }
    }

    /**
     * Run a single connect attempt against the currently-active protocol.
     * Shows optimistic 'connecting' status, polls intermediate states, and maps
     * the typed command result to a ConnectOutcome. The caller owns isLoading,
     * userIntent and the reconnect counters — this primitive only attempts.
     */
    async function runAttempt(): Promise<ConnectOutcome> {
      connectError.value = null

      // Optimistically set connecting status for instant UI feedback.
      // TODO: consider replacing polling with Tauri events for real-time state sync:
      // Rust side: app.emit("vpn-status", &conn_info) on each ConnectionStatus transition
      // TS side: listen("vpn-status", (e) => { connectionInfo.value = e.payload })
      // This would eliminate polling and give instant UI updates for all transitions
      // (connecting → verifying_connection → connected), but adds complexity
      // (event setup, specta typing, dedup with refreshStatus). Current approach
      // (optimistic status + 500ms poll) is good enough for now.
      connectionInfo.value = {
        status: 'connecting',
        protocol: null,
        server_endpoint: null,
        assigned_ip: null,
        connected_at: null,
        last_packet_received: null,
        stats: { tx_bytes: 0, rx_bytes: 0, tx_bytes_per_sec: 0, rx_bytes_per_sec: 0 },
        dns_ok: true,
      }

      // Poll status during connect to show intermediate states (connecting → verifying_connection)
      const pollId = setInterval(() => refreshStatus(), 500)

      try {
        const settings = useSettingsStore()
        const splitMode = settings.splitMode !== 'all' ? settings.splitMode : null
        const selectedApps =
          splitMode && settings.selectedApps.length > 0 ? [...settings.selectedApps] : null

        const result = await commands.connect(splitMode, selectedApps)
        await refreshStatus()
        if (result.status === 'error') {
          connectError.value = result.error
          return { ok: false, error: result.error }
        }
        return { ok: true }
      } finally {
        clearInterval(pollId)
      }
    }

    /**
     * Probe order for auto-select: the user-defined priority (settings.protocolOrder)
     * limited to protocols that actually have a cached config, plus any available
     * protocol missing from the saved order, with the last-active (= last working)
     * protocol moved to the front so reconnects skip straight to it.
     */
    function autoOrder(): Protocol[] {
      const settings = useSettingsStore()
      const available = availableProtocols.value
      const ordered = settings.protocolOrder.filter((p) => available.includes(p))
      for (const p of available) {
        if (!ordered.includes(p)) ordered.push(p)
      }
      const remembered = config.value?.protocol
      if (remembered && ordered.includes(remembered)) {
        return [remembered, ...ordered.filter((p) => p !== remembered)]
      }
      return ordered
    }

    /**
     * Auto-select: try each protocol in autoOrder() until one connects, then stay
     * on it. Aborts cleanly if the user disconnects mid-cycle (abortGen). Leaves
     * connectError set to the last failure so the caller can react (e.g. re-provision
     * the peer when every protocol reports verify_failed).
     */
    async function runAutoCycle() {
      const order = autoOrder()
      const gen = abortGen
      for (let i = 0; i < order.length; i++) {
        if (gen !== abortGen) return
        const proto = order[i]!
        attempt.value = { protocol: proto, index: i + 1, total: order.length }
        await setProtocol(proto)
        const outcome = await runAttempt()
        // User disconnected while this attempt was in flight — disconnect() already
        // tore the tunnel down, so don't claim success or keep probing.
        if (gen !== abortGen) {
          attempt.value = null
          return
        }
        if (outcome.ok) {
          attempt.value = null
          reconnectAttempts.value = 0
          return
        }
      }
      attempt.value = null
      error.value = t('vpn.allProtocolsFailed')
    }

    async function connect() {
      // Re-entrancy guard: bail as a pure no-op if a connect cycle is already
      // running, so overlapping callers don't each spin a probe + poll loop.
      if (connectInFlight) return
      if (!hasConfig.value || !config.value) {
        error.value = t('vpn.noActiveConfig')
        return
      }
      connectInFlight = true
      isLoading.value = true
      error.value = null
      userIntent.value = 'connected'

      try {
        const settings = useSettingsStore()
        if (settings.autoSelect && availableProtocols.value.length > 1) {
          await runAutoCycle()
        } else {
          const outcome = await runAttempt()
          if (outcome.ok) {
            reconnectAttempts.value = 0
          } else if (outcome.error.code !== 'busy') {
            // 'busy' = the backend already has a transition in progress; it's not a
            // user-facing failure, so don't surface it as an error.
            error.value = outcome.error.message
          }
        }
      } catch (e) {
        error.value = String(e)
      } finally {
        attempt.value = null
        isLoading.value = false
        connectInFlight = false
      }
    }

    async function reconnect() {
      await disconnect()
      await connect()
    }

    async function disconnect() {
      isLoading.value = true
      error.value = null

      userIntent.value = 'disconnected'
      abortGen++
      attempt.value = null
      reconnectAttempts.value = 0
      if (reconnectTimeoutId) {
        clearTimeout(reconnectTimeoutId)
        reconnectTimeoutId = null
      }

      try {
        const result = await commands.disconnect()
        if (result.status === 'error') {
          error.value = result.error
        }
        await refreshStatus()
      } catch (e) {
        error.value = String(e)
      } finally {
        isLoading.value = false
      }
    }

    async function refreshStatus() {
      try {
        const result = await commands.getConnectionInfo()
        if (result.status === 'ok') {
          const prevStatus = connectionInfo.value?.status
          connectionInfo.value = result.data

          if (
            prevStatus === 'connected' &&
            result.data.status === 'disconnected' &&
            userIntent.value === 'connected'
          ) {
            handleUnexpectedDisconnect()
          }
        }
      } catch (e) {
        console.error('Failed to refresh status:', e)
      }
    }

    function handleUnexpectedDisconnect() {
      if (!hasConfig.value) {
        reconnectAttempts.value = 0
        return
      }

      if (reconnectAttempts.value >= MAX_RECONNECT_ATTEMPTS) {
        reconnectAttempts.value = 0
        if (onReconnectFailed) {
          onReconnectFailed()
        } else {
          error.value = t('vpn.connectionLost')
        }
        return
      }

      reconnectAttempts.value++
      const delay = Math.min(1000 * Math.pow(2, reconnectAttempts.value - 1), 10000)
      error.value = t('vpn.reconnecting', {
        current: reconnectAttempts.value,
        max: MAX_RECONNECT_ATTEMPTS,
      })

      reconnectTimeoutId = setTimeout(async () => {
        reconnectTimeoutId = null
        try {
          await connect()
        } catch {
          // Will be retried on next refreshStatus cycle if still disconnected
        }
      }, delay)
    }

    async function setProtocol(protocol: Protocol) {
      error.value = null
      try {
        const result = await commands.setActiveProtocol(protocol)
        if (result.status === 'error') {
          error.value = result.error
          return
        }
        await loadConfig()
      } catch (e) {
        error.value = String(e)
      }
    }

    /**
     * Forget the remembered protocol: reset the active protocol to the first
     * preferred one that's available (per AUTO_PROTOCOL_ORDER), so auto-select
     * probes from the top of the order again instead of sticking to the last
     * working protocol.
     */
    async function resetProtocolPreference() {
      const settings = useSettingsStore()
      const fallback =
        settings.protocolOrder.find((p) => availableProtocols.value.includes(p)) ??
        availableProtocols.value[0]
      if (fallback) await setProtocol(fallback)
    }

    function setOnReconnectFailed(cb: (() => void) | null) {
      onReconnectFailed = cb
    }

    return {
      config,
      connectionInfo,
      isLoading,
      error,
      connectError,
      attempt,
      isConnected,
      hasConfig,
      isAndroid,
      deviceId,
      deviceName,
      connectionStatus,
      activeProtocol,
      availableProtocols,
      initPlatform,
      cleanup,
      loadConfig,
      setActiveConfig,
      clearConfig,
      connect,
      disconnect,
      reconnect,
      refreshStatus,
      setProtocol,
      resetProtocolPreference,
      setOnReconnectFailed,
    }
  },
  {
    persist: false,
  },
)

export type { ConnectionInfo, ConfigSafe, ConnectError }
