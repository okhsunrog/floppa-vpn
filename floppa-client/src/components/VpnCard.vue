<script setup lang="ts">
import { onMounted, ref, computed, watch } from 'vue'
import vpnConnectedImg from '../assets/vpn-connected.png?inline'
import vpnDisconnectedImg from '../assets/vpn-disconnected.png?inline'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getMeQuery } from 'floppa-web-shared/client/@pinia/colada.gen'
import {
  createMyPeer,
  getMyPeerByDevice,
  getMyPeerConfig,
  getMyVlessConfig,
  getPublicConfig,
  upsertMyInstallation,
} from 'floppa-web-shared/client/sdk.gen'
import {
  ConnectionIndicator,
  describeError,
  formatBytes,
  formatDuration,
  formatSpeed,
  isApiError,
} from 'floppa-web-shared'
import { platform } from '@tauri-apps/plugin-os'
import { useNow } from '@vueuse/core'
import { useVpnStore } from '../stores/vpnStore'
import { describeVpnError } from '../utils/vpnErrors'
import type { CycleOutcome, Protocol } from '../bindings'
import { useSettingsStore } from '../stores/settingsStore'
import { usePermissionsStore } from '../stores/permissionsStore'
import { isUnhandledOutcome, type HandledOutcome } from '../utils/outcomes'

const { t } = useI18n()
const vpn = useVpnStore()
const settingsStore = useSettingsStore()
const permissions = usePermissionsStore()
const setupErrorKey = ref<SyncErrorKey | null>(null)
const setupErrorDetail = ref<string | null>(null)
const setupPhase = ref<'idle' | 'offline'>('idle')
let syncGeneration = 0

const setupError = computed<string | null>(() => {
  if (setupErrorKey.value) return t(setupErrorKey.value, { detail: setupErrorDetail.value ?? '' })
  return null
})

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
 * True while we are talking to the server about re-provisioning a peer.
 *
 * Owned here rather than in the store because it is not a tunnel state: the tunnel is genuinely
 * idle during it.
 */
const reprovisioning = ref(false)

/**
 * The one thing the button reads.
 *
 * `reprovisioning` used to reach only the label, so the button read "Connecting" with no spinner
 * and stayed clickable — the same split between label and spinner this whole design exists to
 * remove, just the other way round. Anything that should make the button look busy has to arrive
 * through here.
 */
const busy = computed(() => vpn.isBusy || reprovisioning.value)

const { data: me, refresh: refreshMe, error: meQueryError } = useQuery(getMeQuery())

// Clear offline banner when server becomes reachable again
watch(meQueryError, (err) => {
  if (!err && setupPhase.value === 'offline') {
    setupPhase.value = 'idle'
  }
})

/** Locale keys a failed server sync can be reported under. */
type SyncErrorKey = 'vpn.noSubscription' | 'vpn.peerLimitReached' | 'vpn.peerCreateFailed'

type SyncResult =
  | { outcome: 'ok' }
  | { outcome: 'error'; errorKey: SyncErrorKey; detail?: string }
  | { outcome: 'offline' }

/**
 * Whether this device has no peer for `protocol`, as opposed to us failing to find out.
 *
 * Only a 404 means "no peer". A network failure leaves `data` empty too, and reading that as
 * "no peer" is what used to make an offline start create a duplicate — or, in the reconnect
 * path, re-provision a peer that was never gone.
 */
type PeerLookup = { found: 'yes'; id: number } | { found: 'no' } | { found: 'unknown' }

/** The protocols that are backed by a per-device peer row on the server (VLESS is per-user). */
type WgFamilyProtocol = Exclude<Protocol, 'vless'>

async function lookupPeer(protocol: WgFamilyProtocol): Promise<PeerLookup> {
  const { data: peer, response } = await getMyPeerByDevice({
    path: { device_id: vpn.deviceId! },
    query: { protocol },
  })
  if (peer) return { found: 'yes', id: peer.id }
  return response?.status === 404 ? { found: 'no' } : { found: 'unknown' }
}

/**
 * Fetch (and optionally create) the wg-family peer for `protocol`, loading its config into the
 * VPN store. `allowCreate=false` only loads a pre-existing peer (so the secondary protocol never
 * consumes a peer slot). Returns an error outcome on subscription/limit failures during create.
 */
async function syncWgFamilyPeer(
  protocol: WgFamilyProtocol,
  allowCreate: boolean,
): Promise<SyncResult> {
  const lookup = await lookupPeer(protocol)

  if (lookup.found === 'yes') {
    const { data: configStr } = await getMyPeerConfig({
      path: { id: lookup.id },
      throwOnError: true,
    })
    await vpn.importConfig(configStr)
    return { outcome: 'ok' }
  }

  if (lookup.found === 'unknown') return { outcome: 'offline' }

  if (!allowCreate) return { outcome: 'ok' }

  if (!me.value?.subscription) {
    return { outcome: 'error', errorKey: 'vpn.noSubscription' }
  }

  // Not the Pinia Colada mutation: it re-throws the error body untyped, and `response` — the
  // only thing that tells a server refusal from no server at all — is lost on the way.
  const {
    data: created,
    error,
    response,
  } = await createMyPeer({
    body: {
      device_id: vpn.deviceId,
      device_name: vpn.deviceName,
      protocol,
    },
    throwOnError: false,
  })
  if (created) {
    await vpn.importConfig(created.config)
    return { outcome: 'ok' }
  }
  if (!response) return { outcome: 'offline' }

  // Error codes as `floppa-server/src/admin/error.rs` names them. `isApiError` narrows the code
  // to the `ApiErrorCode` union, so a misspelled case here is a type error rather than a branch
  // that never matches.
  if (isApiError(error)) {
    switch (error.error) {
      case 'no_active_subscription':
        return { outcome: 'error', errorKey: 'vpn.noSubscription' }
      case 'peer_limit_reached':
        return { outcome: 'error', errorKey: 'vpn.peerLimitReached' }
    }
  }
  return {
    outcome: 'error',
    errorKey: 'vpn.peerCreateFailed',
    detail: describeError(error, `HTTP ${response.status}`, t),
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
          platform: platform(),
          app_version: __APP_VERSION__,
        },
      })
    } catch {
      // Non-critical — continue with peer sync even if installation upsert fails
    }

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

    // 3. Fetch VLESS config (per-user, no peer slot). A server that does not offer VLESS says so
    //    with `vless_not_configured`, which is not a failure of ours; anything else is worth a
    //    log line but must not fail the sync — the wg-family peer above is what matters.
    try {
      const { data: vlessConfig, error: vlessError } = await getMyVlessConfig()
      if (vlessConfig?.uri) {
        await vpn.importConfig(vlessConfig.uri)
      } else if (isApiError(vlessError) && vlessError.error !== 'vless_not_configured') {
        console.warn('[VpnCard] VLESS config refused:', vlessError.error, vlessError.message)
      }
    } catch (e) {
      console.warn('[VpnCard] VLESS config unavailable:', e)
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
      setupErrorDetail.value = result.detail ?? null
      break
    case 'offline':
      setupPhase.value = 'offline'
      break
  }
}

async function setupAutoPeer() {
  setupErrorKey.value = null
  setupErrorDetail.value = null

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
  if (vpn.isConnected || vpn.isCancellable) {
    await vpn.disconnect()
    return
  }
  await handleOutcome(await vpn.connect())
}

/**
 * React to a cycle that ended without connecting.
 *
 * The one thing this cannot do is decide *why* it failed — that comes typed from the actor. A
 * protocol whose verification failed is the signal that its peer may have been deleted
 * server-side, and it is looked up by name rather than by "whichever protocol was tried last",
 * which is what the old code assumed and got wrong whenever the order had more than one entry.
 */
async function handleOutcome(outcome: CycleOutcome | null) {
  if (!outcome) return

  const verifyFailed =
    outcome.outcome === 'exhausted'
      ? outcome.failures.find((f) => f.error.kind === 'verify_failed')?.protocol
      : outcome.outcome === 'lost_gave_up'
        ? outcome.protocol
        : undefined

  if (outcome.outcome === 'unwind_failed') {
    vpn.setError({ kind: 'unwind_failed' })
    return
  }

  // Nothing to re-provision: the probes failed for reasons a new peer would not fix. Show the
  // last probe's typed error — it is the one for the protocol the user most likely cares about,
  // and every kind it can carry has words in the locale.
  if (outcome.outcome === 'exhausted' && !verifyFailed) {
    const failure = outcome.failures.at(-1)
    if (failure && failure.error.kind !== 'cancelled') {
      vpn.setError({ kind: 'attempt_failed', failure })
    }
    return
  }

  // VLESS has no per-device peer to look up: its config is per-user and never deleted by a
  // peer removal, so a failed VLESS verification is not a "peer gone" signal.
  if (verifyFailed === 'vless') {
    vpn.setError({ kind: 'connection_failed' })
    return
  }
  if (!verifyFailed || !vpn.deviceId) return

  reprovisioning.value = true
  try {
    console.info('[VpnCard] checking whether the peer still exists on the server...')
    const lookup = await lookupPeer(verifyFailed)
    switch (lookup.found) {
      case 'no':
        console.info('[VpnCard] peer is gone, recreating it')
        await setupAutoPeer()
        if (vpn.hasConfig) {
          console.info('[VpnCard] got a new config, reconnecting')
          await vpn.connect()
        }
        break
      case 'yes':
        console.info('[VpnCard] the peer exists, so the problem is elsewhere')
        vpn.setError({ kind: 'connection_failed' })
        break
      case 'unknown':
        console.warn('[VpnCard] could not reach the server to check the peer')
        vpn.setError({ kind: 'connection_failed' })
        break
    }
  } catch (e) {
    console.error('[VpnCard] peer check failed:', e)
    vpn.setError({ kind: 'connection_failed' })
  } finally {
    reprovisioning.value = false
  }
}

/**
 * Outcomes of cycles nobody asked for.
 *
 * When a live tunnel drops, the actor reconnects on its own — there is no caller awaiting that
 * epoch, so if it eventually gives up, nothing would ever surface it. This watcher is the only
 * consumer of those. It remembers the `{ epoch, outcome }` pair it handled so a state refresh
 * cannot make it fire twice, and only that pair: the epoch alone is shared with the `connected`
 * that preceded the loss, and keying on it was what silenced `lost_gave_up` for good. Whether a
 * command of ours is in flight is irrelevant — `handleConnect` only ever receives the outcome
 * that ends its own cycle, never the loss of a tunnel that cycle already delivered.
 */
let handledOutcome: HandledOutcome | null = null
watch(
  () => vpn.lastOutcome,
  async (outcome) => {
    if (!outcome) return
    const epoch = vpn.state.epoch
    if (!isUnhandledOutcome(handledOutcome, epoch, outcome)) return
    handledOutcome = { epoch, outcome: outcome.outcome }

    if (outcome.outcome === 'lost_gave_up') {
      console.info('[VpnCard] the tunnel dropped and reconnecting gave up')
      await handleOutcome(outcome)
    }
  },
)

/**
 * The label. Its spinner comes from `busy`, which is derived from the same two values this reads,
 * so the two cannot describe different situations — which is what used to produce a spinner
 * sitting next to the word "Connect".
 */
const buttonLabel = computed(() => {
  if (reprovisioning.value) return t('vpn.connecting')
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

      <div v-if="vpn.isConnected" class="flex flex-col gap-1 text-sm text-[var(--ui-text-muted)]">
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
        :disabled="!vpn.hasConfig || vpn.phase === 'unknown' || reprovisioning"
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
              vpn.manualProtocol === proto
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
