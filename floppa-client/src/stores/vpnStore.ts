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
import type { ConnectionStatus } from 'floppa-web-shared'
import { useSettingsStore } from './settingsStore'
import { describeUnknown } from '../utils/errors'
import { isUnhandledOutcome, type HandledOutcome } from '../utils/outcomes'
import type { VpnError } from '../utils/vpnErrors'
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

    /**
     * How many of our commands are in flight — distinct from the tunnel itself being busy.
     *
     * A counter, not a flag: Cancel starts a disconnect while a connect is still waiting on its
     * own cycle, and the connect's `finally` cleared the flag while the disconnect was still
     * running. Nothing visibly broke, because the phase covers that window, but a flag that says
     * "no request in flight" while one is means the next thing to read it will be wrong.
     */
    const inFlight = ref(0)
    const requesting = computed(() => inFlight.value > 0)

    /** See `VpnError`. Cleared by the next request; set through `setError` from outside. */
    const error = ref<VpnError | null>(null)

    function setError(next: VpnError) {
      error.value = next
    }

    const isAndroid = ref(false)
    const deviceId = ref<string | null>(null)
    const deviceName = ref<string | null>(null)

    /** True once `init()` has learnt the platform, the device identity and the first snapshot. */
    const ready = ref(false)

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

    /**
     * The last outcome anyone has taken responsibility for.
     *
     * Kept here rather than in the card that reads it: `last_outcome` is sticky until the next
     * accepted intent, and the record used to be a local of `VpnCard`, so returning to the
     * dashboard re-handled an outcome from minutes ago — a peer lookup every time, and a connect
     * nobody asked for if that lookup said the peer was gone.
     */
    const handled = ref<HandledOutcome | null>(null)

    function markOutcomeHandled(epoch: number, outcome: CycleOutcome) {
      handled.value = { epoch, outcome: outcome.outcome, seq: state.value.seq }
    }

    /**
     * The outcome of a cycle nobody has dealt with, or null.
     *
     * A caller that awaited its own cycle marks what it got, so what is left here is what nobody
     * asked for: a tunnel that dropped and could not be brought back, a rebuild that ran out of
     * protocols, a teardown that could not be confirmed.
     */
    const unhandledOutcome = computed<CycleOutcome | null>(() => {
      const outcome = state.value.last_outcome
      if (!outcome) return null
      return isUnhandledOutcome(handled.value, state.value.epoch, outcome) ? outcome : null
    })

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

    let initialising: Promise<void> | null = null

    /**
     * Learn the platform and device identity, subscribe to state changes, then seed from a
     * direct read.
     *
     * In that order deliberately: subscribing first means no update can slip through the gap
     * between reading and listening. Polling is gone — a webview that has been backgrounded has
     * its timers throttled, so an interval here was never a dependable clock. The clock lives in
     * Rust, where it keeps running.
     *
     * Idempotent: main.ts starts it at app scope, and any screen that needs the device identity
     * awaits the same promise rather than re-running the platform probes.
     */
    function init(): Promise<void> {
      initialising ??= (async () => {
        await initPlatform()
        try {
          // The subscription lives as long as the app: the store is never disposed, so the
          // unlisten handle has no caller and is not kept.
          await events.tunnelStateChanged.listen((e) => apply(e.payload))
        } catch (e) {
          console.error('[vpnStore] failed to subscribe to tunnel state:', e)
        }
        await refresh()
        ready.value = true
      })()
      return initialising
    }

    /**
     * The split rules a connect from here would ask for.
     *
     * Sorted and deduplicated, the way `TunnelParams::new` promises on the Rust side: `apps` is
     * built by pushing and splicing as the user taps, so unticking and reticking one app changed
     * the order without changing the set — and the actor compares params by value, so "the same
     * tunnel" turned into a rebuild.
     */
    function params(): TunnelParams {
      const settings = useSettingsStore()
      const apps = settings.splitMode === 'all' ? [] : [...new Set(settings.selectedApps)].sort()
      return { split_mode: settings.splitMode, apps }
    }

    /**
     * Do the settings ask for a different tunnel than the one that is running?
     *
     * Derived from the snapshot rather than remembered: the running tunnel publishes the rules it
     * was actually built with, so this survives a remount, a moment of `retrying` and a trip to
     * another page — all of which used to clear the component flag that stood in for it while the
     * tunnel carried on with the old rules. False when nothing is running and when the rules are
     * unknown (an adopted tunnel whose owner does not report them): there is nothing to compare,
     * and guessing would nag about a tunnel that may well be correct.
     */
    const splitDirty = computed(() => {
      const running = state.value.params
      if (!isConnected.value || !running) return false
      return !sameParams(running, params())
    })

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

      return await request(() => commands.tunnelSetIntentUp(order, params()))
    }

    /**
     * Also the cancel button: stopping an attempt is a change of intent, not a separate action.
     *
     * Returns its outcome like `connect` does. It used to throw the result of `tunnelAwaitCycle`
     * away, so `unwind_failed` — a teardown that could not be confirmed, which is the one moment
     * the machine may be left configured — reached nobody, and the UI showed a plain
     * "Disconnected".
     */
    async function disconnect(): Promise<CycleOutcome | null> {
      return await request(() => commands.tunnelSetIntentDown())
    }

    /**
     * Send an intent and wait for the cycle it starts.
     *
     * One body for both directions, because they differ only in the intent: accept it, mirror the
     * new state, wait for a terminal outcome, mirror again. Every failure lands in `error` —
     * including an `actor_gone` from the wait, which used to return null silently while the same
     * error from the accept was shown.
     */
    async function request(
      send: () => Promise<
        { status: 'ok'; data: { epoch: number } } | { status: 'error'; error: VpnError }
      >,
    ): Promise<CycleOutcome | null> {
      error.value = null
      inFlight.value += 1
      try {
        const accepted = await send()
        if (accepted.status === 'error') {
          error.value = accepted.error
          return null
        }
        await refresh()

        const outcome = await commands.tunnelAwaitCycle(accepted.data.epoch)
        await refresh()
        if (outcome.status === 'error') {
          error.value = outcome.error
          return null
        }
        // Whoever called this is about to deal with it, so the unsolicited-outcome watcher must
        // not deal with it as well.
        markOutcomeHandled(accepted.data.epoch, outcome.data)
        return outcome.data
      } catch (e) {
        error.value = { kind: 'unexpected', detail: describeUnknown(e) }
        return null
      } finally {
        inFlight.value -= 1
      }
    }

    /**
     * Take the tunnel down and forget every stored config.
     *
     * The command does the waiting: it issues a Down that must leave nothing running — including
     * a tunnel the always-on toggle started — and wipes only once the actor is genuinely idle. So
     * this replaces a `disconnect()`, it does not follow one.
     *
     * What it removes is the previous account's private keys, its VLESS URI and the autostart
     * bundle. Without it they stayed on the device: a second account on the same phone that could
     * not reach the server connected under the first account's identity, and always-on could
     * bring the logged-out account's tunnel back by itself.
     */
    async function forgetConfigs(): Promise<boolean> {
      error.value = null
      inFlight.value += 1
      try {
        const result = await commands.clearConfigs()
        if (result.status === 'error') {
          error.value = result.error
          return false
        }
        handled.value = null
        await refresh()
        return true
      } catch (e) {
        error.value = { kind: 'unexpected', detail: describeUnknown(e) }
        return false
      } finally {
        inFlight.value -= 1
      }
    }

    /** Store a config. Storing is not choosing: this does not change what the next connect uses. */
    async function importConfig(raw: string) {
      error.value = null
      try {
        const result = await commands.importConfig(raw)
        if (result.status === 'error') {
          error.value = result.error
          return
        }
        await refresh()
      } catch (e) {
        error.value = { kind: 'unexpected', detail: describeUnknown(e) }
      }
    }

    /**
     * Forget which protocol last worked, so the next connect probes from the top again.
     * Returns whether it happened; a failure is also recorded in `error`.
     */
    async function forgetPreferred(): Promise<boolean> {
      error.value = null
      try {
        const result = await commands.forgetPreferredProtocol()
        if (result.status === 'error') {
          error.value = { kind: 'unexpected', detail: 'forget_preferred_protocol was refused' }
          return false
        }
        await refresh()
        return true
      } catch (e) {
        error.value = { kind: 'unexpected', detail: describeUnknown(e) }
        return false
      }
    }

    return {
      state,
      error,
      setError,
      requesting,
      phase,
      isConnected,
      isBusy,
      isCancellable,
      isAndroid,
      deviceId,
      deviceName,
      ready,
      availableProtocols,
      hasConfig,
      activeProtocol,
      manualProtocol,
      attempt,
      retry,
      lastOutcome,
      unhandledOutcome,
      markOutcomeHandled,
      splitDirty,
      params,
      init,
      refresh,
      connect,
      disconnect,
      forgetConfigs,
      importConfig,
      forgetPreferred,
    }
  },
  { persist: false },
)

/** The actor's own "same tunnel" test, on the shape the snapshot publishes. */
function sameParams(a: TunnelParams, b: TunnelParams): boolean {
  return (
    a.split_mode === b.split_mode &&
    a.apps.length === b.apps.length &&
    a.apps.every((app, i) => app === b.apps[i])
  )
}

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
    params: null,
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
export type { VpnError, VpnErrorKind } from '../utils/vpnErrors'
