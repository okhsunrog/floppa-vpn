import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import {
  commands,
  events,
  type CycleOutcome,
  type Phase,
  type Protocol,
  type TunnelParams,
  type TunnelState,
} from '../bindings'
import type { UnlistenFn } from '@tauri-apps/api/event'
import type { ConnectionStatus } from 'floppa-web-shared'
import { useSettingsStore } from './settingsStore'
import { platform } from '@tauri-apps/plugin-os'

/**
 * `ConnectionStatus` is a hand-written copy of the generated `Phase`, and has to be: it lives in
 * the shared package, which the client depends on rather than the other way round, so it cannot
 * import from `bindings.ts`. What it *can* have is this — a check that the two are still the same
 * set, which fails to compile the moment a phase is added or removed on either side.
 */
type Identical<A, B> = [A] extends [B] ? ([B] extends [A] ? true : never) : never
const _phaseMatchesConnectionStatus: Identical<Phase, ConnectionStatus> = true
void _phaseMatchesConnectionStatus

/**
 * A read-only mirror of the tunnel state the Rust actor publishes.
 *
 * Everything about the tunnel arrives as one value, so the phase, the probe progress and the retry
 * countdown cannot disagree with each other. That is what makes the old spinner-with-"Connect"
 * button unrepresentable: there is no second source left to disagree with.
 *
 * Deleted from here and deliberately not replaced:
 *
 * - `isLoading` — the phase already says whether the tunnel is busy
 * - `connectError` — the outcome is typed and arrives with the state
 * - `attempt`, `abortGen`, `runAutoCycle`, `autoOrder` — the actor walks the protocol order
 * - `userIntent`, `reconnectAttempts`, `reconnectTimeoutId` — auto-reconnect is what the actor
 *   does when an Up intent outlives a failure; nothing here implements it
 * - `setProtocol` — the order is part of the request, and which protocol worked is recorded by
 *   the side that watched it work
 */
export const useVpnStore = defineStore(
  'vpn',
  () => {
    /** The mirror. Replaced wholesale by refresh(); never edited piecemeal. */
    const state = ref<TunnelState>(emptyState())

    /** True only while a command is in flight — distinct from the tunnel itself being busy. */
    const requesting = ref(false)

    /** Errors that are not about the tunnel: importing a config, reaching the actor. */
    const error = ref<string | null>(null)

    const isAndroid = ref(false)
    const deviceId = ref<string | null>(null)
    const deviceName = ref<string | null>(null)

    const phase = computed(() => state.value.phase)
    const isConnected = computed(() => phase.value === 'connected')
    /**
     * Busy as the actor reports it, or because a command of ours is still in flight.
     *
     * Only the second half is decided here, and it has to be: `requesting` is about this webview's
     * pending call, which the actor has no view of. Which phases count as work in progress used to
     * be decided here too, from a copy of the Rust list — the two agreed, and nothing made them.
     */
    const isBusy = computed(() => requesting.value || state.value.busy)
    const isCancellable = computed(() => state.value.cancellable)

    const availableProtocols = computed(() => state.value.configs.available)
    const hasConfig = computed(() => availableProtocols.value.length > 0)
    /** What is running, or failing that what last worked. */
    const activeProtocol = computed(
      () => state.value.protocol ?? state.value.configs.preferred ?? availableProtocols.value[0],
    )
    /**
     * What a manual-mode connect would use: the user's pick from the switcher when we still hold
     * a config for it, else whatever `activeProtocol` says. Read by the switcher's highlight and
     * by `connect()`, so the card can never show one protocol and request another.
     */
    const manualProtocol = computed(() => {
      const picked = useSettingsStore().manualProtocol
      return picked && availableProtocols.value.includes(picked) ? picked : activeProtocol.value
    })
    const attempt = computed(() => state.value.attempt)
    const retry = computed(() => state.value.retry)
    const lastOutcome = computed(() => state.value.last_outcome)

    async function initPlatform() {
      try {
        isAndroid.value = platform() === 'android'
      } catch (e) {
        console.error('[vpnStore] failed to detect the platform:', e)
      }
      try {
        const result = await commands.getDeviceId()
        if (result.status === 'ok') deviceId.value = result.data
      } catch (e) {
        console.error('[vpnStore] failed to get the device id:', e)
      }
      try {
        deviceName.value = await commands.getDeviceName()
      } catch (e) {
        console.error('[vpnStore] failed to get the device name:', e)
      }
    }

    /** Pull the published snapshot. On the Rust side this is a local read — no IPC, no lock. */
    async function refresh() {
      try {
        apply(await commands.tunnelGetState())
      } catch (e) {
        console.error('[vpnStore] failed to read the tunnel state:', e)
      }
    }

    /**
     * Accept a snapshot if it is newer than the one we hold.
     *
     * `seq` only ever increases, which is what makes the seed and the subscription safe to race:
     * whichever arrives second is simply dropped if it is older. Without it, a slow reply to the
     * initial read could overwrite a pushed update that already superseded it.
     */
    function apply(next: TunnelState) {
      if (next.seq >= state.value.seq) state.value = next
    }

    let unlisten: UnlistenFn | null = null

    /**
     * Subscribe to state changes, then seed from a direct read.
     *
     * In that order deliberately: subscribing first means no update can slip through the gap
     * between reading and listening. Polling is gone — a webview that has been backgrounded has
     * its timers throttled, so an interval here was never a dependable clock. The clock lives in
     * Rust, where it keeps running.
     */
    async function init() {
      if (unlisten) return
      try {
        unlisten = await events.tunnelStateChanged.listen((e) => apply(e.payload))
      } catch (e) {
        console.error('[vpnStore] failed to subscribe to tunnel state:', e)
      }
      await refresh()
    }

    function params(): TunnelParams {
      const settings = useSettingsStore()
      const apps = settings.splitMode === 'all' ? [] : [...settings.selectedApps]
      return { split_mode: settings.splitMode, apps }
    }

    /**
     * Ask for a tunnel, and wait for the request to reach a terminal outcome.
     *
     * In auto-select mode the order sent is the user's priority list as-is. Narrowing it to
     * protocols we actually hold a config for, and moving whichever one last worked to the front,
     * is the actor's job — it is the side that knows both. In manual mode it is exactly the one
     * protocol the switcher shows.
     */
    async function connect(): Promise<CycleOutcome | null> {
      const settings = useSettingsStore()
      const order = settings.autoSelect
        ? [...settings.protocolOrder]
        : [manualProtocol.value].filter((p): p is Protocol => !!p)

      error.value = null
      requesting.value = true
      try {
        const accepted = await commands.tunnelSetIntentUp(order, params())
        if (accepted.status === 'error') {
          error.value = accepted.error.kind
          return null
        }
        await refresh()

        const outcome = await commands.tunnelAwaitCycle(accepted.data.epoch)
        await refresh()
        return outcome.status === 'ok' ? outcome.data : null
      } catch (e) {
        error.value = String(e)
        return null
      } finally {
        requesting.value = false
      }
    }

    /** Also the cancel button: stopping an attempt is a change of intent, not a separate action. */
    async function disconnect() {
      error.value = null
      requesting.value = true
      try {
        const accepted = await commands.tunnelSetIntentDown()
        if (accepted.status === 'error') {
          error.value = accepted.error.kind
          return
        }
        await refresh()
        await commands.tunnelAwaitCycle(accepted.data.epoch)
        await refresh()
      } catch (e) {
        error.value = String(e)
      } finally {
        requesting.value = false
      }
    }

    /** Store a config. Storing is not choosing: this does not change what the next connect uses. */
    async function importConfig(raw: string) {
      error.value = null
      try {
        const result = await commands.importConfig(raw)
        if (result.status === 'error') {
          error.value = result.error.kind
          return
        }
        await refresh()
      } catch (e) {
        error.value = String(e)
      }
    }

    /** Forget which protocol last worked, so the next connect probes from the top again. */
    async function forgetPreferred() {
      await commands.forgetPreferredProtocol()
      await refresh()
    }

    async function clearConfigs() {
      error.value = null
      requesting.value = true
      try {
        const result = await commands.clearConfigs()
        if (result.status === 'error') error.value = result.error.kind
        await refresh()
      } finally {
        requesting.value = false
      }
    }

    return {
      state,
      error,
      requesting,
      phase,
      isConnected,
      isBusy,
      isCancellable,
      isAndroid,
      deviceId,
      deviceName,
      availableProtocols,
      hasConfig,
      activeProtocol,
      manualProtocol,
      attempt,
      retry,
      lastOutcome,
      initPlatform,
      init,
      refresh,
      connect,
      disconnect,
      importConfig,
      forgetPreferred,
      clearConfigs,
    }
  },
  { persist: false },
)

function emptyState(): TunnelState {
  return {
    seq: 0,
    // Not 'disconnected': before the first snapshot arrives we have no idea whether a tunnel is
    // running, and saying otherwise is what made an active tunnel flash as down on app start.
    phase: 'unknown',
    // Matches what Rust publishes for 'unknown': pending, and nothing to cancel.
    busy: true,
    cancellable: false,
    intent: 'down',
    epoch: 0,
    intent_order: [],
    protocol: null,
    adopted: false,
    attempt: null,
    retry: null,
    server_endpoint: null,
    assigned_ip: null,
    connected_at: null,
    last_packet_received: null,
    stats: { tx_bytes: 0, rx_bytes: 0, tx_bytes_per_sec: 0, rx_bytes_per_sec: 0 },
    last_outcome: null,
    configs: { available: [], preferred: null, summaries: [] },
    backend_reachable: false,
  }
}

export type { TunnelState, CycleOutcome, Protocol }
