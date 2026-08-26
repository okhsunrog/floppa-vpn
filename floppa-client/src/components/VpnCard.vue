<script setup lang="ts">
import { onMounted, computed, watch } from 'vue'
import vpnConnectedImg from '../assets/vpn-connected.png?inline'
import vpnDisconnectedImg from '../assets/vpn-disconnected.png?inline'
import { useI18n } from 'vue-i18n'
import { ConnectionIndicator, formatBytes, formatDuration, formatSpeed } from 'floppa-web-shared'
import { useNow } from '@vueuse/core'
import { useVpnStore } from '../stores/vpnStore'
import { describeVpnError } from '../utils/vpnErrors'
import type { Protocol } from '../bindings'
import { useSettingsStore } from '../stores/settingsStore'
import { usePermissionsStore } from '../stores/permissionsStore'
import { usePeerProvisioning } from '../composables/usePeerProvisioning'
import { needsAttention } from '../utils/outcomes'

const { t } = useI18n()
const vpn = useVpnStore()
const settingsStore = useSettingsStore()
const permissions = usePermissionsStore()
const { setupPhase, setupError, meQueryError, noteServerReachable, setupAutoPeer, handleOutcome } =
  usePeerProvisioning()

/** The store's typed error, worded. */
const vpnErrorText = computed<string | null>(() =>
  vpn.error ? describeVpnError(vpn.error, t) : null,
)

// Show prompts after first successful connection on Android
watch(
  () => vpn.isConnected,
  async (connected, wasConnected) => {
    if (!connected || wasConnected || !vpn.isAndroid) return
    await permissions.checkPromptsAfterConnection()
  },
)

/**
 * The one thing the button reads.
 *
 * It used to also carry "we are talking to the server about a peer", from a flag that reached
 * only the label — so the button read "Connecting" with no spinner and stayed clickable. That
 * work is in Rust now and the tunnel's own phase covers it: a repair that leads anywhere ends in
 * a reconnect, which is busy for the ordinary reason. Anything that should make the button look
 * busy still has to arrive through here.
 */
const busy = computed(() => vpn.isBusy)

// Clear offline banner when server becomes reachable again
watch(meQueryError, (err) => {
  if (!err) noteServerReachable()
})

onMounted(async () => {
  // Started at app scope in main.ts; this only waits for the device identity and first snapshot.
  await vpn.init()

  // Preload app list for settings page (non-blocking)
  if (vpn.isAndroid) settingsStore.loadApps()

  if (vpn.deviceId) {
    await setupAutoPeer()
  }
})

async function handleConnect() {
  // Cancelling an attempt and disconnecting a tunnel are the same request: a change of intent.
  // Its outcome matters too — a teardown that could not be confirmed is the one case where the
  // machine may be left configured, and it used to be discarded.
  if (vpn.isConnected || vpn.isCancellable) {
    await handleOutcome(await vpn.disconnect())
    return
  }
  await handleOutcome(await vpn.connect())
}

/**
 * Rebuild the tunnel with the split rules the settings now ask for.
 *
 * Asking again is enough — the actor sees a tunnel that no longer satisfies the request and
 * rebuilds it — but the outcome has to be handled, or a rebuild that fails says nothing.
 */
async function reconnectForSplit() {
  await handleOutcome(await vpn.connect())
}

/**
 * Outcomes of cycles nobody asked for.
 *
 * When a live tunnel drops, the actor reconnects on its own — there is no caller awaiting that
 * epoch, so if it eventually gives up, nothing would ever surface it. This watcher is the only
 * consumer of those. Which ones have already been dealt with is the store's business now: the
 * record used to live here, and it was reset by every remount, so returning to the dashboard
 * re-handled a `lost_gave_up` from minutes ago.
 *
 * Every ending without a tunnel is handled, not only `lost_gave_up`: a reconnect that ran out of
 * protocols reports `exhausted`, and a teardown that could not be confirmed reports
 * `unwind_failed`. Both have words in the locale and neither used to reach anyone.
 */
watch(
  () => vpn.unhandledOutcome,
  async (outcome) => {
    if (!outcome) return
    vpn.markOutcomeHandled(outcome)
    if (needsAttention(outcome)) {
      console.info(`[VpnCard] a cycle nobody awaited ended: ${outcome.outcome}`)
    }
    // Every outcome is offered, not only the ones with something to say. A cycle that *connected*
    // can still have stepped over a protocol whose peer is gone, and repairing it is silent work
    // with nothing to show a user — so gating this on "needs attention" meant the repair never ran
    // for the reconnects that happen with nobody watching, which is exactly when it is needed.
    // `handleOutcome` decides; for an ordinary success it decides to do nothing.
    await handleOutcome(outcome)
  },
)

/**
 * The label. Its spinner comes from `busy`, which is derived from the same value this reads, so
 * the two cannot describe different situations — which is what used to produce a spinner sitting
 * next to the word "Connect".
 */
const buttonLabel = computed(() => {
  switch (vpn.phase) {
    // Before anything has been observed we have no answer to give, so the button says so rather
    // than inviting an action whose effect we cannot predict.
    case 'unknown':
      return t('status.unknown')
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

// A ticking clock, so the duration counts up on its own. It used to be a function call in the
// template, which only re-ran when some other part of the state happened to change.
const now = useNow({ interval: 1000 })

const connectionDuration = computed(() => {
  const since = vpn.state.connected_at
  if (!since) return '--'
  return formatDuration(Math.max(0, Math.floor(now.value.getTime() / 1000 - since)))
})

function formatLastPacket(secs: number | null | undefined): string {
  if (secs == null || secs < 0) return '--'
  return formatDuration(secs, { trimZeroSeconds: true })
}

function selectProtocol(proto: Protocol) {
  // In manual mode the request carries exactly one protocol: the pick. It is remembered across
  // launches, but it is not a reordering of the auto-select priority — that list is its own
  // setting and stays as the user left it.
  settingsStore.manualProtocol = proto
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
      <!--
        No device identity: `getDeviceId` failed, so there is no peer to provision and the
        Connect button is disabled with nothing on screen explaining why. Reachable on desktop
        when the config directory cannot be written.
      -->
      <UAlert
        v-if="vpn.ready && !vpn.deviceId"
        color="error"
        variant="soft"
        icon="i-lucide-fingerprint"
        :title="t('vpn.noDeviceIdentity')"
        class="w-full max-w-sm"
      />

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

      <!--
        Backing off between passes. Shown because the actor keeps working here with nothing else
        on screen to say so: without it a reconnect looks identical to a hung app.
      -->
      <div v-else-if="vpn.retry" class="flex flex-col items-center gap-1">
        <span class="text-sm text-[var(--ui-text-muted)]">
          {{ t('vpn.reconnecting', { current: vpn.retry.pass, max: vpn.retry.max }) }}
        </span>
        <span class="text-xs text-[var(--ui-text-muted)]">
          {{ t('vpn.retryingIn', { seconds: Math.ceil(vpn.retry.resume_in_ms / 1000) }) }}
        </span>
      </div>

      <!-- Active protocol — auto-select mode only; manual mode shows it via the switcher -->
      <UBadge
        v-else-if="vpn.isConnected && settingsStore.autoSelect && vpn.state.protocol"
        color="neutral"
        variant="subtle"
      >
        {{ t('vpn.connectedVia', { protocol: t(`vpn.${vpn.state.protocol}`) }) }}
      </UBadge>

      <!--
        Contract fields the snapshot has always published and nobody read. `backend_reachable` is
        the only honest signal that the `:vpn` service has stopped answering while the tunnel is
        still believed to be up — the card otherwise shows Connected with frozen counters —
        and `adopted` says the tunnel was not started from here, which is what makes an
        always-on start explicable rather than surprising.
      -->
      <UAlert
        v-if="vpn.isConnected && !vpn.state.backend_reachable"
        color="warning"
        variant="soft"
        icon="i-lucide-plug-zap"
        :title="t('vpn.serviceNotAnswering')"
        class="mt-2 w-full max-w-sm"
      />

      <div v-if="vpn.isConnected" class="flex flex-col gap-1 text-sm text-[var(--ui-text-muted)]">
        <span v-if="vpn.state.adopted" class="text-xs">{{ t('vpn.adoptedTunnel') }}</span>
        <span v-if="vpn.state.assigned_ip"> IP: {{ vpn.state.assigned_ip }} </span>
        <span v-if="vpn.state.server_endpoint">
          {{ t('vpn.server') }}: {{ vpn.state.server_endpoint }}
        </span>
        <span>{{ t('vpn.duration') }}: {{ connectionDuration }}</span>
        <span class="inline-flex items-center justify-center gap-1.5">
          {{ t('vpn.lastActivity') }}:
          <span class="size-2 rounded-full" :class="healthDotClass" />
          {{ formatLastPacket(vpn.state.last_packet_received) }}
        </span>
      </div>

      <!--
        The running tunnel does not route what the settings say. Derived from the rules the
        tunnel publishes, so it survives a remount and a moment of retrying — the flag it
        replaces was cleared by both, while the tunnel carried on with the old rules.
      -->
      <UAlert
        v-if="vpn.splitDirty"
        color="warning"
        variant="soft"
        icon="i-lucide-split"
        :title="t('settings.changesApplyOnReconnect')"
        class="mt-2 w-full max-w-sm"
      >
        <template #actions>
          <UButton
            :label="t('settings.reconnect')"
            color="warning"
            variant="outline"
            size="xs"
            :loading="busy"
            @click="reconnectForSplit"
          />
        </template>
      </UAlert>

      <UAlert
        v-if="vpnErrorText"
        color="error"
        :title="vpnErrorText"
        class="mt-2 w-full max-w-sm"
      />
      <UAlert
        v-else-if="setupError"
        color="warning"
        :title="setupError"
        class="mt-2 w-full max-w-sm"
      />

      <!--
        Whether the button cancels is decided by one value, the same one `handleConnect` branches
        on. Keying this off `vpn.attempt` instead meant a backing-off retry — cancellable, but with
        no attempt in flight — fell through to the main button, which then said "Connecting" while
        actually cancelling.
      -->
      <UButton
        v-if="vpn.isCancellable"
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
        :loading="busy"
        :disabled="!vpn.hasConfig || vpn.phase === 'unknown'"
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
            :disabled="vpn.isConnected || busy"
            class="px-4 py-1.5 text-sm rounded-md transition-all"
            :class="
              vpn.switcherProtocol === proto
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
  <UCard v-if="permissions.showNotificationPrompt" class="mb-4">
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
  <UCard v-if="permissions.showBatteryPrompt" class="mb-4">
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
  <UCard v-if="vpn.isConnected" class="mb-4">
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
