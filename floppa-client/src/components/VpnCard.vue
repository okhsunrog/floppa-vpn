<script setup lang="ts">
import { onMounted, onUnmounted, ref, computed, watch } from 'vue'
import vpnConnectedImg from '../assets/vpn-connected.png?inline'
import vpnDisconnectedImg from '../assets/vpn-disconnected.png?inline'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import { getMeQuery, createMyPeerMutation } from 'floppa-web-shared/client/@pinia/colada.gen'
import {
  getMyPeerByDevice,
  getMyPeerConfig,
  getMyVlessConfig,
  upsertMyInstallation,
} from 'floppa-web-shared/client/sdk.gen'
import { formatBytes, formatSpeed, formatDuration, ConnectionIndicator } from 'floppa-web-shared'
import { platform } from '@tauri-apps/plugin-os'
import { useVpnStore } from '../stores/vpnStore'
import { useSettingsStore } from '../stores/settingsStore'
import { useAndroidPermissions } from '../composables/useAndroidPermissions'

const { t } = useI18n()
const vpn = useVpnStore()
const settingsStore = useSettingsStore()
const permissions = useAndroidPermissions()
const setupErrorKey = ref<string | null>(null)
const setupPhase = ref<'idle' | 'offline'>('idle')
let syncGeneration = 0

const setupError = computed<string | null>(() => {
  if (setupErrorKey.value) return t(setupErrorKey.value)
  return null
})

// Show prompts after first successful connection on Android
watch(
  () => vpn.isConnected,
  async (connected, wasConnected) => {
    if (!connected || wasConnected || !vpn.isAndroid) return
    await permissions.checkPromptsAfterConnection()
  },
)

let statusInterval: ReturnType<typeof setInterval> | null = null

const { data: me, refresh: refreshMe, error: meQueryError } = useQuery(getMeQuery())
const createPeerMut = useMutation(createMyPeerMutation())

// Clear offline banner when server becomes reachable again
watch(meQueryError, (err) => {
  if (!err && setupPhase.value === 'offline') {
    setupPhase.value = 'idle'
  }
})

type SyncResult =
  | { outcome: 'ok' }
  | { outcome: 'error'; errorKey: string }
  | { outcome: 'offline' }

async function doServerSync(): Promise<SyncResult> {
  try {
    await refreshMe()

    // If the server is unreachable, refreshMe silently fails (Pinia Colada
    // doesn't throw). Check query error before proceeding — otherwise
    // getMyPeerByDevice returns { data: undefined } on network error,
    // which looks identical to a 404 and would wrongly revoke cached config.
    if (meQueryError.value) {
      return { outcome: 'offline' }
    }

    // Register/update this device installation
    try {
      await upsertMyInstallation({
        body: {
          device_id: vpn.deviceId!,
          device_name: vpn.deviceName ?? undefined,
          platform: await platform(),
          app_version: __APP_VERSION__,
        },
      })
    } catch {
      // Non-critical — continue with peer sync even if installation upsert fails
    }

    // Remember active protocol before sync (setActiveConfig switches to last-set protocol).
    // On first start (no localStorage, no loaded config), leave null so we default to
    // the first available protocol (VLESS) after sync.
    const prevProtocol =
      localStorage.getItem('preferredProtocol') ?? (vpn.hasConfig ? vpn.activeProtocol : null)

    // 1. Fetch WireGuard peer
    const { data: peer } = await getMyPeerByDevice({
      path: { device_id: vpn.deviceId! },
    })

    if (peer) {
      const { data: configStr } = await getMyPeerConfig({
        path: { id: peer.id },
        throwOnError: true,
      })
      await vpn.setActiveConfig(configStr)
    } else {
      // No peer — try to create
      if (!me.value?.subscription) {
        return { outcome: 'error', errorKey: 'vpn.noSubscription' }
      }

      try {
        const response = await createPeerMut.mutateAsync({
          body: {
            device_id: vpn.deviceId,
            device_name: vpn.deviceName,
          },
        })
        await vpn.setActiveConfig(response.config)
      } catch (e: unknown) {
        const errorCode = (e as Record<string, unknown>)?.error
        if (errorCode === 'no_active_subscription' || errorCode === 'subscription_expired') {
          return { outcome: 'error', errorKey: 'vpn.noSubscription' }
        }
        return { outcome: 'error', errorKey: 'vpn.peerLimitReached' }
      }
    }

    // 2. Fetch VLESS config (if available on server)
    try {
      const { data: vlessConfig } = await getMyVlessConfig()
      if (vlessConfig?.uri) {
        await vpn.setActiveConfig(vlessConfig.uri)
      }
    } catch {
      // VLESS not available on server — skip silently
    }

    // 3. Restore previously active protocol (or default to first available)
    if (prevProtocol && vpn.availableProtocols.includes(prevProtocol)) {
      await vpn.setProtocol(prevProtocol)
    } else if (vpn.availableProtocols.length > 0) {
      await vpn.setProtocol(vpn.availableProtocols[0]!)
    }

    return { outcome: 'ok' }
  } catch {
    return { outcome: 'offline' }
  }
}

function applySyncResult(result: SyncResult) {
  switch (result.outcome) {
    case 'ok':
      setupPhase.value = 'idle'
      break
    case 'error':
      setupPhase.value = 'idle'
      setupErrorKey.value = result.errorKey
      break
    case 'offline':
      setupPhase.value = 'offline'
      break
  }
}

async function setupAutoPeer() {
  setupErrorKey.value = null

  const thisGeneration = ++syncGeneration

  const timeoutPromise = new Promise<'timeout'>((resolve) =>
    setTimeout(() => resolve('timeout'), 5000),
  )

  const syncPromise = doServerSync()
  const winner = await Promise.race([
    syncPromise.then((result) => ({ type: 'sync' as const, result })),
    timeoutPromise.then(() => ({ type: 'timeout' as const })),
  ])

  // Stale guard — discard if a newer call was made
  if (thisGeneration !== syncGeneration) return

  if (winner.type === 'sync') {
    applySyncResult(winner.result)
  } else {
    setupPhase.value = 'offline'

    // Let the background sync finish; apply only if it succeeded
    syncPromise.then((result) => {
      if (thisGeneration !== syncGeneration) return
      if (setupPhase.value !== 'offline') return
      if (result.outcome === 'ok' || result.outcome === 'error') {
        applySyncResult(result)
      }
    })
  }
}

async function handleReconnectFailed() {
  if (!vpn.deviceId) return
  await setupAutoPeer()
  // If sync got us a new config, auto-connect
  if (vpn.hasConfig) {
    await vpn.connect()
  }
}

onMounted(async () => {
  await vpn.initPlatform()

  // Preload app list for settings page (non-blocking)
  if (vpn.isAndroid) settingsStore.loadApps()

  vpn.setOnReconnectFailed(handleReconnectFailed)

  await vpn.loadConfig()
  await vpn.refreshStatus()

  if (vpn.deviceId) {
    await setupAutoPeer()
  }

  statusInterval = setInterval(async () => {
    // Always poll on Android to detect surviving :vpn process after app kill.
    // On desktop the UI process owns the tunnel, so only poll when connected.
    if (vpn.isConnected || vpn.isAndroid) {
      await vpn.refreshStatus()
    }
  }, 1000)
})

onUnmounted(() => {
  if (statusInterval) {
    clearInterval(statusInterval)
  }
  vpn.setOnReconnectFailed(null)
  vpn.cleanup()
})

async function handleConnect() {
  if (vpn.isConnected) {
    await vpn.disconnect()
    return
  }

  await vpn.connect()

  // If connection verification failed, check with server whether our peer still exists
  if (vpn.error?.includes('verification failed') && vpn.deviceId) {
    // Keep UI in loading state while we check server and potentially recreate
    vpn.error = null
    vpn.isLoading = true
    try {
      console.info('[VpnCard] Connection verification failed, checking peer with server...')
      const { data: peer } = await getMyPeerByDevice({
        path: { device_id: vpn.deviceId },
      })
      if (!peer) {
        console.info('[VpnCard] Peer not found on server, recreating...')
        await setupAutoPeer()
        if (vpn.hasConfig) {
          console.info('[VpnCard] New config obtained, reconnecting...')
          await vpn.connect()
        }
      } else {
        console.info('[VpnCard] Peer exists on server, connection issue is elsewhere')
        vpn.error = t('vpn.connectionFailed')
      }
    } finally {
      vpn.isLoading = false
    }
  }
}

const buttonLabel = computed(() => {
  const s = vpn.connectionInfo?.status
  switch (s) {
    case 'connecting':
      return t('vpn.connecting')
    case 'verifying_connection':
      return t('vpn.verifyingConnection')
    case 'disconnecting':
      return t('vpn.disconnecting')
    case 'connected':
      return t('vpn.disconnect')
    default:
      return t('vpn.connect')
  }
})

function getConnectionDuration(): string {
  if (!vpn.connectionInfo?.connected_at) return '--'
  const seconds = Math.floor(Date.now() / 1000 - vpn.connectionInfo.connected_at)
  return formatDuration(seconds)
}

function formatLastPacket(secs: number | null | undefined): string {
  if (secs == null || secs < 0) return '--'
  if (secs < 60) return `${secs}s`
  const m = Math.floor(secs / 60)
  const s = secs % 60
  return s > 0 ? `${m}m ${s}s` : `${m}m`
}

function selectProtocol(proto: string) {
  vpn.setProtocol(proto)
  localStorage.setItem('preferredProtocol', proto)
}

const healthDotClass = computed(() => {
  const secs = vpn.connectionInfo?.last_packet_received
  if (secs == null || secs < 0 || secs > 150) return 'bg-red-500'
  if (secs > 120) return 'bg-yellow-500'
  return 'bg-green-500'
})
</script>

<template>
  <!-- Connection Card -->
  <UCard class="mb-4">
    <div class="flex flex-col items-center text-center gap-3">
      <!-- Offline mode banner -->
      <UAlert
        v-if="setupPhase === 'offline'"
        color="warning"
        variant="soft"
        icon="i-lucide-wifi-off"
        :title="t('vpn.offlineMode')"
        :description="vpn.hasConfig ? t('vpn.offlineModeHint') : t('vpn.offlineModeNoConfig')"
        class="w-full max-w-sm"
      >
        <template #actions>
          <UButton
            :label="t('vpn.retry')"
            icon="i-lucide-refresh-cw"
            color="warning"
            variant="outline"
            size="xs"
            @click="setupAutoPeer()"
          />
        </template>
      </UAlert>

      <div
        :class="[
          'status-circle',
          {
            connected: vpn.isConnected,
            connecting:
              vpn.connectionStatus === 'connecting' || vpn.connectionStatus === 'disconnecting',
          },
        ]"
      >
        <img
          :src="vpn.isConnected ? vpnConnectedImg : vpnDisconnectedImg"
          alt=""
          class="size-20 object-contain transition-all duration-300"
        />
      </div>

      <ConnectionIndicator
        :status="vpn.connectionStatus"
        show-label
        class="text-xl font-semibold"
      />

      <div
        v-if="vpn.isConnected && vpn.connectionInfo"
        class="flex flex-col gap-1 text-sm text-[var(--ui-text-muted)]"
      >
        <span v-if="vpn.connectionInfo.assigned_ip">
          IP: {{ vpn.connectionInfo.assigned_ip }}
        </span>
        <span v-if="vpn.connectionInfo.server_endpoint">
          Server: {{ vpn.connectionInfo.server_endpoint }}
        </span>
        <span>{{ t('vpn.duration') }}: {{ getConnectionDuration() }}</span>
        <span class="inline-flex items-center justify-center gap-1.5">
          {{ t('vpn.lastActivity') }}:
          <span class="size-2 rounded-full" :class="healthDotClass" />
          {{ formatLastPacket(vpn.connectionInfo.last_packet_received) }}
        </span>
      </div>

      <UAlert v-if="vpn.error" color="error" :title="vpn.error" class="mt-2 w-full max-w-sm" />
      <UAlert
        v-else-if="setupError"
        color="warning"
        :title="setupError"
        class="mt-2 w-full max-w-sm"
      />

      <UButton
        :label="buttonLabel"
        :icon="vpn.isConnected ? 'i-lucide-power' : 'i-lucide-play'"
        :color="vpn.isConnected ? 'error' : 'success'"
        :loading="vpn.isLoading"
        :disabled="!vpn.hasConfig"
        size="lg"
        class="w-full max-w-[200px] mt-2"
        @click="handleConnect"
      />

      <!-- Protocol toggle (only show when multiple protocols available) -->
      <div v-if="vpn.availableProtocols.length > 1" class="mt-3">
        <div class="text-xs text-[var(--ui-text-muted)] mb-1.5">{{ t('vpn.protocol') }}</div>
        <div class="inline-flex rounded-lg bg-[var(--ui-bg-elevated)] p-0.5">
          <button
            v-for="proto in vpn.availableProtocols"
            :key="proto"
            :disabled="vpn.isConnected || vpn.isLoading"
            class="px-4 py-1.5 text-sm rounded-md transition-all"
            :class="
              vpn.activeProtocol === proto
                ? 'bg-[var(--ui-bg)] text-[var(--ui-text)] shadow-sm font-medium'
                : 'text-[var(--ui-text-muted)] hover:text-[var(--ui-text)]'
            "
            @click="selectProtocol(proto)"
          >
            {{ t(`vpn.${proto}`) }}
          </button>
        </div>
      </div>
    </div>
  </UCard>

  <!-- Notification Prompt -->
  <UCard v-if="permissions.showNotificationPrompt.value" class="mb-4">
    <div class="flex flex-col gap-3">
      <div class="flex items-start gap-3">
        <UIcon name="i-lucide-bell-off" class="text-2xl text-yellow-500 shrink-0 mt-0.5" />
        <p class="text-sm">{{ t('settings.notificationPrompt') }}</p>
      </div>
      <div class="flex gap-2 justify-end">
        <UButton
          :label="t('update.dismiss')"
          color="neutral"
          variant="ghost"
          size="sm"
          @click="permissions.dismissNotificationPrompt"
        />
        <UButton
          :label="t('settings.enableNotifications')"
          color="warning"
          size="sm"
          @click="permissions.handleNotificationPrompt"
        />
      </div>
    </div>
  </UCard>

  <!-- Battery Optimization Prompt -->
  <UCard v-if="permissions.showBatteryPrompt.value" class="mb-4">
    <div class="flex flex-col gap-3">
      <div class="flex items-start gap-3">
        <UIcon name="i-lucide-battery-warning" class="text-2xl text-yellow-500 shrink-0 mt-0.5" />
        <p class="text-sm">{{ t('settings.batteryPrompt') }}</p>
      </div>
      <div class="flex gap-2 justify-end">
        <UButton
          :label="t('update.dismiss')"
          color="neutral"
          variant="ghost"
          size="sm"
          @click="permissions.dismissBatteryPrompt"
        />
        <UButton
          :label="t('settings.disableBatteryOptimization')"
          color="warning"
          size="sm"
          @click="permissions.handleBatteryPrompt"
        />
      </div>
    </div>
  </UCard>

  <!-- Traffic Stats -->
  <UCard v-if="vpn.isConnected && vpn.connectionInfo" class="mb-4">
    <template #header>
      <span class="font-semibold">{{ t('vpn.traffic') }}</span>
    </template>
    <div class="grid grid-cols-2 gap-4">
      <div class="flex items-center gap-3 p-3 bg-[var(--ui-bg-elevated)] rounded-lg">
        <UIcon name="i-lucide-arrow-up" class="text-2xl text-green-500" />
        <div class="flex flex-col">
          <span class="font-semibold text-lg">
            {{ formatBytes(vpn.connectionInfo.stats.tx_bytes) }}
          </span>
          <span class="text-xs text-[var(--ui-text-muted)]">
            {{ formatSpeed(vpn.connectionInfo.stats.tx_bytes_per_sec) }}
          </span>
        </div>
      </div>
      <div class="flex items-center gap-3 p-3 bg-[var(--ui-bg-elevated)] rounded-lg">
        <UIcon name="i-lucide-arrow-down" class="text-2xl text-[var(--ui-primary)]" />
        <div class="flex flex-col">
          <span class="font-semibold text-lg">
            {{ formatBytes(vpn.connectionInfo.stats.rx_bytes) }}
          </span>
          <span class="text-xs text-[var(--ui-text-muted)]">
            {{ formatSpeed(vpn.connectionInfo.stats.rx_bytes_per_sec) }}
          </span>
        </div>
      </div>
    </div>
  </UCard>
</template>

<style scoped>
.status-circle {
  width: 120px;
  height: 120px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--ui-bg-elevated);
  border: 4px solid var(--ui-border);
  transition: all 0.3s ease;
}

.status-circle.connected {
  background: color-mix(in srgb, var(--color-green-500) 20%, transparent);
  border-color: var(--color-green-500);
}

.status-circle.connecting {
  background: color-mix(in srgb, var(--color-yellow-500) 20%, transparent);
  border-color: var(--color-yellow-500);
  animation: pulse 1.5s ease-in-out infinite;
}

@keyframes pulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.7;
  }
}
</style>
