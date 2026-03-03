<script setup lang="ts">
import { onMounted, onUnmounted, ref, computed, watch } from 'vue'
import vpnConnectedImg from '../assets/vpn-connected.png?inline'
import vpnDisconnectedImg from '../assets/vpn-disconnected.png?inline'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import { getMeQuery, createMyPeerMutation } from 'floppa-web-shared/client/@pinia/colada.gen'
import { getMyPeerByDevice, getMyPeerConfig } from 'floppa-web-shared/client/sdk.gen'
import { formatBytes, formatSpeed, formatDuration, ConnectionIndicator } from 'floppa-web-shared'
import { useVpnStore } from '../stores/vpnStore'
import { useSettingsStore } from '../stores/settingsStore'
import { commands } from '../bindings'

const { t } = useI18n()
const vpn = useVpnStore()
const settingsStore = useSettingsStore()
const setupErrorKey = ref<string | null>(null)
const setupErrorMsg = ref<string | null>(null)
const setupPhase = ref<'idle' | 'offline'>('idle')
let syncGeneration = 0

const setupError = computed<string | null>(() => {
  if (setupErrorKey.value) return t(setupErrorKey.value)
  return setupErrorMsg.value
})

const showBatteryPrompt = ref(false)
const batteryPromptDismissed = ref(localStorage.getItem('battery_prompt_dismissed') === 'true')
const showNotificationPrompt = ref(false)
const notificationPromptDismissed = ref(localStorage.getItem('notification_prompt_dismissed') === 'true')

// Show prompts after first successful connection on Android
watch(() => vpn.isConnected, async (connected, wasConnected) => {
  if (!connected || wasConnected || !vpn.isAndroid) return

  if (!batteryPromptDismissed.value) {
    try {
      const result = await commands.isBatteryOptimizationDisabled()
      if (result.status === 'ok' && !result.data) {
        showBatteryPrompt.value = true
      }
    } catch { /* ignore */ }
  }

  if (!notificationPromptDismissed.value) {
    try {
      const result = await commands.areNotificationsEnabled()
      if (result.status === 'ok' && !result.data) {
        showNotificationPrompt.value = true
      }
    } catch { /* ignore */ }
  }
})

async function handleBatteryOptimization() {
  try {
    await commands.requestDisableBatteryOptimization()
  } catch { /* ignore */ }
  showBatteryPrompt.value = false
  batteryPromptDismissed.value = true
  localStorage.setItem('battery_prompt_dismissed', 'true')
}


function dismissBatteryPrompt() {
  showBatteryPrompt.value = false
  batteryPromptDismissed.value = true
  localStorage.setItem('battery_prompt_dismissed', 'true')
}

async function handleEnableNotifications() {
  try {
    await commands.openNotificationSettings()
  } catch { /* ignore */ }
  showNotificationPrompt.value = false
  notificationPromptDismissed.value = true
  localStorage.setItem('notification_prompt_dismissed', 'true')
}

function dismissNotificationPrompt() {
  showNotificationPrompt.value = false
  notificationPromptDismissed.value = true
  localStorage.setItem('notification_prompt_dismissed', 'true')
}

let statusInterval: ReturnType<typeof setInterval> | null = null

const { data: me, refresh: refreshMe, error: meQueryError } = useQuery(getMeQuery())
const createPeerMut = useMutation(createMyPeerMutation())

// Clear offline banner when server becomes reachable again
watch(meQueryError, (err) => {
  if (!err && setupPhase.value === 'offline') {
    setupPhase.value = 'idle'
  }
})

type SyncResult = { outcome: 'ok' } | { outcome: 'error'; errorKey: string } | { outcome: 'offline' }

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

    const { data: peer } = await getMyPeerByDevice({
      path: { device_id: vpn.deviceId! },
    })

    if (peer) {
      const { data: configStr } = await getMyPeerConfig({ path: { id: peer.id }, throwOnError: true })
      await vpn.setActiveConfig(configStr)
      return { outcome: 'ok' }
    }

    // Backend reachable but peer not found (404)
    if (vpn.hasConfig) {
      await vpn.clearConfig()
      return { outcome: 'error', errorKey: 'vpn.configRevoked' }
    }

    // No peer, no config — try to create
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
      return { outcome: 'ok' }
    } catch {
      return { outcome: 'error', errorKey: 'vpn.peerLimitReached' }
    }
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
  setupErrorMsg.value = null

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

onMounted(async () => {
  await vpn.initPlatform()

  // Preload app list for settings page (non-blocking)
  if (vpn.isAndroid) settingsStore.loadApps()

  await vpn.loadConfig()
  await vpn.refreshStatus()

  if (vpn.deviceId) {
    await setupAutoPeer()
  }

  statusInterval = setInterval(async () => {
    if (vpn.isConnected) {
      await vpn.refreshStatus()
    }
  }, 2000)
})

onUnmounted(() => {
  if (statusInterval) {
    clearInterval(statusInterval)
  }
  vpn.cleanup()
})

async function handleConnect() {
  if (vpn.isConnected) {
    await vpn.disconnect()
  } else {
    await vpn.connect()
  }
}

function getConnectionDuration(): string {
  if (!vpn.connectionInfo?.connected_at) return '--'
  const seconds = Math.floor(Date.now() / 1000 - vpn.connectionInfo.connected_at)
  return formatDuration(seconds)
}
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
              connecting: vpn.connectionStatus === 'connecting' || vpn.connectionStatus === 'disconnecting',
            },
          ]">
          <img
            :src="vpn.isConnected ? vpnConnectedImg : vpnDisconnectedImg"
            alt=""
            class="size-20 object-contain transition-all duration-300" />
        </div>

        <ConnectionIndicator :status="vpn.connectionStatus" show-label class="text-xl font-semibold" />

        <div
          v-if="vpn.isConnected && vpn.connectionInfo"
          class="flex flex-col gap-1 text-sm text-[var(--ui-text-muted)]">
          <span v-if="vpn.connectionInfo.assigned_ip">
            IP: {{ vpn.connectionInfo.assigned_ip }}
          </span>
          <span v-if="vpn.connectionInfo.server_endpoint">
            Server: {{ vpn.connectionInfo.server_endpoint }}
          </span>
          <span>Duration: {{ getConnectionDuration() }}</span>
        </div>

        <UAlert v-if="vpn.error" color="error" :title="vpn.error" class="mt-2 w-full max-w-sm" />
        <UAlert v-else-if="setupError" color="warning" :title="setupError" class="mt-2 w-full max-w-sm" />

        <UButton
          :label="vpn.isConnected ? t('vpn.disconnect') : t('vpn.connect')"
          :icon="vpn.isConnected ? 'i-lucide-power' : 'i-lucide-play'"
          :color="vpn.isConnected ? 'error' : 'success'"
          :loading="vpn.isLoading"
          :disabled="!vpn.hasConfig"
          size="lg"
          class="w-full max-w-[200px] mt-2"
          @click="handleConnect" />
    </div>
  </UCard>

  <!-- Notification Prompt -->
  <UCard v-if="showNotificationPrompt" class="mb-4">
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
          @click="dismissNotificationPrompt"
        />
        <UButton
          :label="t('settings.enableNotifications')"
          color="warning"
          size="sm"
          @click="handleEnableNotifications"
        />
      </div>
    </div>
  </UCard>

  <!-- Battery Optimization Prompt -->
  <UCard v-if="showBatteryPrompt" class="mb-4">
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
          @click="dismissBatteryPrompt"
        />
        <UButton
          :label="t('settings.disableBatteryOptimization')"
          color="warning"
          size="sm"
          @click="handleBatteryOptimization"
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
