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
  getPublicConfig,
  upsertMyInstallation,
} from 'floppa-web-shared/client/sdk.gen'
import { formatBytes, formatSpeed, formatDuration, ConnectionIndicator } from 'floppa-web-shared'
import { platform } from '@tauri-apps/plugin-os'
import { useVpnStore } from '../stores/vpnStore'
import type { Protocol } from '../bindings'
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

/**
 * True while we are talking to the server about re-provisioning a peer.
 *
 * Owned here rather than in the store because it is not a tunnel state: the tunnel is genuinely
 * idle during it. It still has to make the button look busy, which is exactly why it feeds the
 * same computed as the phase rather than a second, independent flag.
 */
const reprovisioning = ref(false)

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

/**
 * Fetch (and optionally create) the wg-family peer for `protocol`, loading its config into the
 * VPN store. `allowCreate=false` only loads a pre-existing peer (so the secondary protocol never
 * consumes a peer slot). Returns an error outcome on subscription/limit failures during create.
 */
async function syncWgFamilyPeer(protocol: Protocol, allowCreate: boolean): Promise<SyncResult> {
  const { data: peer } = await getMyPeerByDevice({
    path: { device_id: vpn.deviceId! },
    query: { protocol },
  })

  if (peer) {
    const { data: configStr } = await getMyPeerConfig({
      path: { id: peer.id },
      throwOnError: true,
    })
    await vpn.importConfig(configStr)
    return { outcome: 'ok' }
  }

  if (!allowCreate) return { outcome: 'ok' }

  if (!me.value?.subscription) {
    return { outcome: 'error', errorKey: 'vpn.noSubscription' }
  }

  try {
    const response = await createPeerMut.mutateAsync({
      body: {
        device_id: vpn.deviceId,
        device_name: vpn.deviceName,
        protocol,
      },
    })
    await vpn.importConfig(response.config)
    return { outcome: 'ok' }
  } catch (e: unknown) {
    const errorCode = (e as Record<string, unknown>)?.error
    if (errorCode === 'no_active_subscription' || errorCode === 'subscription_expired') {
      return { outcome: 'error', errorKey: 'vpn.noSubscription' }
    }
    return { outcome: 'error', errorKey: 'vpn.peerLimitReached' }
  }
}

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
    // Source of truth is the persisted active_protocol (loaded into config on mount).
    // On first start (no loaded config) leave null so we default to the first available
    // protocol after sync — AmneziaWG, which is listed first.
    const prevProtocol = vpn.hasConfig ? vpn.activeProtocol : null

    // AmneziaWG is the default wg-family protocol when the server offers it; WireGuard otherwise.
    let amneziaAvailable = false
    try {
      const { data: pub } = await getPublicConfig()
      amneziaAvailable = pub?.amneziawg_available ?? false
    } catch {
      // Couldn't reach /config — fall back to plain WireGuard.
    }
    const primary = amneziaAvailable ? 'amneziawg' : 'wireguard'
    const secondary = primary === 'amneziawg' ? 'wireguard' : 'amneziawg'

    // 1. Provision the primary (default) wg-family peer — must succeed.
    const primaryResult = await syncWgFamilyPeer(primary, true)
    if (primaryResult.outcome === 'error') return primaryResult

    // 2. Also provision the secondary wg-family protocol when the server offers it. A device is a
    //    single peer-limit slot, so holding both WireGuard and AmneziaWG is free — this gives the
    //    user all switcher positions. Best-effort: don't fail the sync if the bonus peer can't be made.
    // The secondary wg protocol is only ever available when AmneziaWG is offered: if it isn't,
    // the primary is WireGuard and the secondary would be the absent AmneziaWG.
    await syncWgFamilyPeer(secondary, amneziaAvailable)

    // 3. Fetch VLESS config (per-user, no peer slot)
    try {
      const { data: vlessConfig } = await getMyVlessConfig()
      if (vlessConfig?.uri) {
        await vpn.importConfig(vlessConfig.uri)
      }
    } catch {
      // VLESS not available on server — skip silently
    }

    // Importing configs no longer changes which protocol a connect would use, so there is
    // nothing to restore here — that used to be necessary only because storing and choosing
    // were the same operation.
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

  await vpn.refresh()

  // One-time on upgrade to auto-select: enable it and forget the previously-used protocol, so the
  // first cycle probes from the configured priority instead of inheriting an old manual pick.
  if (!settingsStore.protocolDefaultsApplied) {
    settingsStore.autoSelect = true
    await vpn.forgetPreferred()
    settingsStore.protocolDefaultsApplied = true
  }

  if (vpn.deviceId) {
    await setupAutoPeer()
  }

  // Mirrors the actor's published snapshot. This is a local read with no IPC and no lock, so
  // polling it is cheap — and it is only a stopgap until the state is pushed as an event.
  statusInterval = setInterval(() => void vpn.refresh(), 1000)
})

onUnmounted(() => {
  if (statusInterval) {
    clearInterval(statusInterval)
  }
})

async function handleConnect() {
  // Cancelling an attempt and disconnecting a tunnel are the same request: a change of intent.
  if (vpn.isConnected || vpn.isCancellable) {
    await vpn.disconnect()
    return
  }

  const outcome = await vpn.connect()

  // Every protocol failed. If any of them failed *verification*, the peer may have been deleted
  // server-side — check that one specifically, rather than whichever protocol happened to be
  // tried last.
  const verifyFailed =
    outcome?.outcome === 'exhausted'
      ? outcome.failures.find((f) => f.error.kind === 'verify_failed')?.protocol
      : undefined

  if (verifyFailed && vpn.deviceId) {
    reprovisioning.value = true
    try {
      console.info('[VpnCard] Connection verification failed, checking peer with server...')
      const { data: peer } = await getMyPeerByDevice({
        path: { device_id: vpn.deviceId },
        query: { protocol: verifyFailed },
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
      reprovisioning.value = false
    }
  }
}

/**
 * The button's label and its spinner both come from `vpn.isBusy`/`vpn.phase`, which arrive in the
 * same snapshot. They cannot disagree — which is what used to produce a spinner sitting next to
 * the word "Connect", because the spinner read one source and the label another.
 */
const buttonLabel = computed(() => {
  if (reprovisioning.value) return t('vpn.connecting')
  switch (vpn.phase) {
    case 'connecting':
      return t('vpn.connecting')
    case 'verifying_connection':
      return t('vpn.verifyingConnection')
    case 'retrying':
      return t('vpn.connecting')
    case 'disconnecting':
      return t('vpn.disconnecting')
    case 'connected':
      return t('vpn.disconnect')
    default:
      return t('vpn.connect')
  }
})

function getConnectionDuration(): string {
  if (!vpn.state.connected_at) return '--'
  const seconds = Math.floor(Date.now() / 1000 - vpn.state.connected_at)
  return formatDuration(seconds)
}

function formatLastPacket(secs: number | null | undefined): string {
  if (secs == null || secs < 0) return '--'
  if (secs < 60) return `${secs}s`
  const m = Math.floor(secs / 60)
  const s = secs % 60
  return s > 0 ? `${m}m ${s}s` : `${m}m`
}

function selectProtocol(proto: Protocol) {
  // In manual mode the request carries exactly one protocol, so choosing one is a local
  // preference — nothing is persisted until it actually connects.
  settingsStore.protocolOrder = [proto, ...settingsStore.protocolOrder.filter((p) => p !== proto)]
}

const healthDotClass = computed(() => {
  const secs = vpn.state.last_packet_received
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
            connecting: vpn.phase === 'connecting' || vpn.phase === 'disconnecting',
          },
        ]"
      >
        <img
          :src="vpn.isConnected ? vpnConnectedImg : vpnDisconnectedImg"
          alt=""
          class="size-20 object-contain transition-all duration-300"
        />
      </div>

      <ConnectionIndicator :status="vpn.phase" show-label class="text-xl font-semibold" />

      <!-- Auto-select probe progress: which protocol we're trying + a stepper -->
      <div v-if="vpn.attempt && vpn.attempt.total > 1" class="flex flex-col items-center gap-2">
        <span class="text-sm text-[var(--ui-text-muted)]">
          {{
            t('vpn.tryingProtocol', {
              protocol: t(`vpn.${vpn.attempt.protocol}`),
              current: vpn.attempt.index,
              total: vpn.attempt.total,
            })
          }}
        </span>
        <div class="flex gap-1.5">
          <span
            v-for="n in vpn.attempt.total"
            :key="n"
            class="size-2 rounded-full transition-colors"
            :class="
              n <= vpn.attempt.index ? 'bg-[var(--ui-primary)]' : 'bg-[var(--ui-bg-elevated)]'
            "
          />
        </div>
      </div>

      <!-- Active protocol — auto-select mode only; manual mode shows it via the switcher -->
      <UBadge
        v-else-if="vpn.isConnected && settingsStore.autoSelect && vpn.state.protocol"
        color="neutral"
        variant="subtle"
      >
        {{ t('vpn.connectedVia', { protocol: t(`vpn.${vpn.state.protocol}`) }) }}
      </UBadge>

      <div
        v-if="vpn.isConnected && vpn.state"
        class="flex flex-col gap-1 text-sm text-[var(--ui-text-muted)]"
      >
        <span v-if="vpn.state.assigned_ip"> IP: {{ vpn.state.assigned_ip }} </span>
        <span v-if="vpn.state.server_endpoint">
          {{ t('vpn.server') }}: {{ vpn.state.server_endpoint }}
        </span>
        <span>{{ t('vpn.duration') }}: {{ getConnectionDuration() }}</span>
        <span class="inline-flex items-center justify-center gap-1.5">
          {{ t('vpn.lastActivity') }}:
          <span class="size-2 rounded-full" :class="healthDotClass" />
          {{ formatLastPacket(vpn.state.last_packet_received) }}
        </span>
      </div>

      <UAlert v-if="vpn.error" color="error" :title="vpn.error" class="mt-2 w-full max-w-sm" />
      <UAlert
        v-else-if="setupError"
        color="warning"
        :title="setupError"
        class="mt-2 w-full max-w-sm"
      />

      <!-- During an auto-select probe the button cancels the cycle -->
      <UButton
        v-if="vpn.attempt"
        :label="t('vpn.cancel')"
        icon="i-lucide-x"
        color="neutral"
        variant="soft"
        size="lg"
        class="w-full max-w-[200px] mt-2"
        @click="vpn.disconnect()"
      />
      <UButton
        v-else
        :label="buttonLabel"
        :icon="vpn.isConnected ? 'i-lucide-power' : 'i-lucide-play'"
        :color="vpn.isConnected ? 'error' : 'success'"
        :loading="vpn.isBusy"
        :disabled="!vpn.hasConfig"
        size="lg"
        class="w-full max-w-[200px] mt-2"
        @click="handleConnect"
      />

      <!-- Protocol toggle — manual mode only (auto-select hides it; the badge above shows the active protocol) -->
      <div v-if="!settingsStore.autoSelect && vpn.availableProtocols.length > 1" class="mt-3">
        <div class="text-xs text-[var(--ui-text-muted)] mb-1.5">{{ t('vpn.protocol') }}</div>
        <div class="inline-flex rounded-lg bg-[var(--ui-bg-elevated)] p-0.5">
          <button
            v-for="proto in vpn.availableProtocols"
            :key="proto"
            :disabled="vpn.isConnected || vpn.isBusy"
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
  <UCard v-if="vpn.isConnected && vpn.state" class="mb-4">
    <template #header>
      <span class="font-semibold">{{ t('vpn.traffic') }}</span>
    </template>
    <div class="grid grid-cols-2 gap-4">
      <div class="flex items-center gap-3 p-3 bg-[var(--ui-bg-elevated)] rounded-lg">
        <UIcon name="i-lucide-arrow-up" class="text-2xl text-green-500" />
        <div class="flex flex-col">
          <span class="font-semibold text-lg">
            {{ formatBytes(vpn.state.stats.tx_bytes) }}
          </span>
          <span class="text-xs text-[var(--ui-text-muted)]">
            {{ formatSpeed(vpn.state.stats.tx_bytes_per_sec ?? 0) }}
          </span>
        </div>
      </div>
      <div class="flex items-center gap-3 p-3 bg-[var(--ui-bg-elevated)] rounded-lg">
        <UIcon name="i-lucide-arrow-down" class="text-2xl text-[var(--ui-primary)]" />
        <div class="flex flex-col">
          <span class="font-semibold text-lg">
            {{ formatBytes(vpn.state.stats.rx_bytes) }}
          </span>
          <span class="text-xs text-[var(--ui-text-muted)]">
            {{ formatSpeed(vpn.state.stats.rx_bytes_per_sec ?? 0) }}
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
