import { computed, reactive } from 'vue'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getMeQuery } from 'floppa-web-shared/client/@pinia/colada.gen'
import { commands, type CycleOutcome, type SyncOutcome } from '../bindings'
import { useVpnStore } from '../stores/vpnStore'
import { planOutcomeResponse } from '../utils/outcomes'

/*
 * The connection card's view of provisioning.
 *
 * Provisioning itself is not here any more. Deciding what a device is entitled to — which peers
 * exist, which may be created, what an unreachable server means — is `floppa-api-client`, in
 * Rust, because the same decision has to be made from `:vpn` with the app closed when a peer is
 * deleted underneath a running tunnel. Two implementations of it disagreed about the details that
 * matter, and the details that matter are the ones that cost a user a peer slot.
 *
 * What is left is what a card is for: a banner while the server is slow, an error in the user's
 * own language, and one call.
 */

/** Locale keys a failed server sync can be reported under. */
export type SyncErrorKey = 'vpn.noSubscription' | 'vpn.peerLimitReached' | 'vpn.peerCreateFailed'

/** How the card reports the last sync: quietly, with an error, or as "offline". */
export type SetupPhase = 'idle' | 'offline'

export interface SetupState {
  phase: SetupPhase
  errorKey: SyncErrorKey | null
  errorDetail: string | null
}

export function emptySetupState(): SetupState {
  return { phase: 'idle', errorKey: null, errorDetail: null }
}

/** The locale key for a refusal, and the detail to interpolate into it. */
function describeFailure(outcome: Extract<SyncOutcome, { outcome: 'failed' }>): {
  key: SyncErrorKey
  detail: string | null
} {
  // A `switch` over the tag: a variant added in Rust arrives through the generated union, and the
  // `never` is what turns forgetting it into a compile error rather than an unlabelled error.
  switch (outcome.error.kind) {
    case 'no_subscription':
      return { key: 'vpn.noSubscription', detail: null }
    case 'peer_limit_reached':
      return { key: 'vpn.peerLimitReached', detail: null }
    case 'create_failed':
      return { key: 'vpn.peerCreateFailed', detail: outcome.error.detail }
    default: {
      const unplanned: never = outcome.error
      return unplanned
    }
  }
}

export function applySyncResult(state: SetupState, result: SyncOutcome) {
  switch (result.outcome) {
    case 'ok':
      state.phase = 'idle'
      break
    case 'failed': {
      const { key, detail } = describeFailure(result)
      state.phase = 'idle'
      state.errorKey = key
      state.errorDetail = detail
      break
    }
    case 'offline':
      state.phase = 'offline'
      break
  }
}

/** How long a sync may take before the card shows the offline banner and lets it finish behind. */
export const SYNC_TIMEOUT_MS = 5000

/**
 * Runs syncs against `state` one at a time, in the sense that only the latest one may write.
 *
 * A sync that outlives the timeout is not abandoned: the banner goes up, and if the sync later
 * finishes with a verdict it is applied — unless a newer run has started since, or the user's
 * retry already took the banner down.
 */
export function createSyncSequencer(state: SetupState, timeoutMs = SYNC_TIMEOUT_MS) {
  let generation = 0

  async function run(sync: Promise<SyncOutcome>): Promise<void> {
    state.errorKey = null
    state.errorDetail = null

    const thisGeneration = ++generation
    const isCurrent = () => thisGeneration === generation

    const timeout = new Promise<{ type: 'timeout' }>((resolve) =>
      setTimeout(() => resolve({ type: 'timeout' }), timeoutMs),
    )
    const winner = await Promise.race([
      sync.then((result) => ({ type: 'sync' as const, result })),
      timeout,
    ])

    if (!isCurrent()) return

    if (winner.type === 'sync') {
      applySyncResult(state, winner.result)
      return
    }

    state.phase = 'offline'
    void sync.then((result) => {
      if (!isCurrent()) return
      if (state.phase !== 'offline') return
      if (result.outcome !== 'offline') applySyncResult(state, result)
    })
  }

  return { run }
}

/**
 * The card's view of provisioning: the offline banner, the setup error, and the two entry points
 * — the sync on mount and on retry, and the reaction to a finished cycle.
 */
export function usePeerProvisioning() {
  const { t } = useI18n()
  const vpn = useVpnStore()

  // Still queried here, and only for the banner: `/me` answering again is how the card learns the
  // server is reachable without making the user press anything.
  const { error: meQueryError } = useQuery(getMeQuery())

  const setup = reactive(emptySetupState())
  const sequencer = createSyncSequencer(setup)

  const setupPhase = computed(() => setup.phase)
  const setupError = computed<string | null>(() =>
    setup.errorKey ? t(setup.errorKey, { detail: setup.errorDetail ?? '' }) : null,
  )

  /** Clear the offline banner once the server answers `/me` again. */
  function noteServerReachable() {
    if (setup.phase === 'offline') setup.phase = 'idle'
  }

  /** Provision this device's peers, showing the offline banner if the server is slow. */
  async function setupAutoPeer(): Promise<void> {
    await sequencer.run(
      commands.syncPeers().then((result): SyncOutcome => {
        if (result.status === 'ok') return result.data
        // The command itself failing is not the server refusing: nothing was learned, which is
        // what "offline" means to the card.
        console.warn('[provisioning] the sync could not be run:', result.error)
        return { outcome: 'offline' }
      }),
    )
  }

  /** React to a finished cycle: say what went wrong, or say nothing. */
  function handleOutcome(outcome: CycleOutcome | null): void {
    if (!outcome) return
    const plan = planOutcomeResponse(outcome)
    if (plan.action === 'show_error') vpn.setError(plan.error)
  }

  return {
    setupPhase,
    setupError,
    meQueryError,
    noteServerReachable,
    setupAutoPeer,
    handleOutcome,
  }
}
