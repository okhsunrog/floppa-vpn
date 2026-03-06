import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { commands, type ConnectionInfo, type WgConfigSafe } from '../bindings'
import { useSettingsStore } from './settingsStore'
import { platform } from '@tauri-apps/plugin-os'
import { i18n } from '../i18n'

const t = i18n.global.t

const MAX_RECONNECT_ATTEMPTS = 3

export const useVpnStore = defineStore(
  'vpn',
  () => {
    const config = ref<WgConfigSafe | null>(null)
    const connectionInfo = ref<ConnectionInfo | null>(null)
    const isLoading = ref(false)
    const error = ref<string | null>(null)
    const isAndroid = ref(false)
    const deviceId = ref<string | null>(null)
    const deviceName = ref<string | null>(null)

    // Auto-reconnect state
    const reconnectAttempts = ref(0)
    let userInitiatedDisconnect = false
    let reconnectTimeoutId: ReturnType<typeof setTimeout> | null = null
    let onReconnectFailed: (() => void) | null = null

    const isConnected = computed(() => connectionInfo.value?.status === 'connected')
    const hasConfig = computed(() => config.value !== null)

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
        const os = await platform()
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
      userInitiatedDisconnect = true
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
      } catch (e) {
        console.error('Failed to load config:', e)
      }
    }

    async function connect() {
      if (!hasConfig.value || !config.value) {
        error.value = t('vpn.noActiveConfig')
        return
      }
      isLoading.value = true
      error.value = null

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
        server_endpoint: null,
        assigned_ip: null,
        connected_at: null,
        last_handshake: null,
        stats: { tx_bytes: 0, rx_bytes: 0, tx_bytes_per_sec: 0, rx_bytes_per_sec: 0 },
      }

      // Poll status during connect to show intermediate states (connecting → verifying_connection)
      const pollId = setInterval(() => refreshStatus(), 500)

      try {
        const settings = useSettingsStore()
        const splitMode = settings.splitMode !== 'all' ? settings.splitMode : null
        const selectedApps =
          splitMode && settings.selectedApps.length > 0 ? [...settings.selectedApps] : null

        const result = await commands.connect(splitMode, selectedApps)
        if (result.status === 'error') {
          error.value = result.error
        } else {
          userInitiatedDisconnect = false
          reconnectAttempts.value = 0
        }
        await refreshStatus()
      } catch (e) {
        error.value = String(e)
      } finally {
        clearInterval(pollId)
        isLoading.value = false
      }
    }

    async function reconnect() {
      await disconnect()
      await connect()
    }

    async function disconnect() {
      isLoading.value = true
      error.value = null

      userInitiatedDisconnect = true
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
            !userInitiatedDisconnect
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

    function setOnReconnectFailed(cb: (() => void) | null) {
      onReconnectFailed = cb
    }

    return {
      config,
      connectionInfo,
      isLoading,
      error,
      isConnected,
      hasConfig,
      isAndroid,
      deviceId,
      deviceName,
      connectionStatus,
      initPlatform,
      cleanup,
      loadConfig,
      setActiveConfig,
      clearConfig,
      connect,
      disconnect,
      reconnect,
      refreshStatus,
      setOnReconnectFailed,
    }
  },
  {
    persist: false,
  },
)

export type { ConnectionInfo, WgConfigSafe }
