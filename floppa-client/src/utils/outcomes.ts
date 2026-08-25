import type { CycleOutcome } from '../bindings'

/** The last outcome acted on: which cycle it belonged to, how it ended, and when it was seen. */
export interface HandledOutcome {
  epoch: number
  outcome: CycleOutcome['outcome']
  /** The snapshot `seq` it was handled at. Diagnostic, and a guard against a rewound snapshot. */
  seq: number
}

/**
 * Whether an outcome published for `epoch` is one nobody has acted on yet.
 *
 * The actor republishes the whole state on every tick, and `last_outcome` stays put until the
 * next accepted intent, so the same outcome arrives many times and has to be deduplicated — but
 * by `{ epoch, outcome }`, not by epoch alone. A cycle that connects and later loses the tunnel
 * for good reports `connected` and then `lost_gave_up` under one epoch (the intent never
 * changed), and keying on the epoch swallowed the second.
 *
 * This lives beside the store rather than in a component: the record used to be a local of
 * `VpnCard`, so returning to the dashboard re-handled a `lost_gave_up` from minutes ago — a peer
 * lookup on every mount, and a connect nobody asked for if the peer really was gone.
 */
export function isUnhandledOutcome(
  handled: HandledOutcome | null,
  epoch: number,
  outcome: CycleOutcome,
): boolean {
  return handled === null || handled.epoch !== epoch || handled.outcome !== outcome.outcome
}

/**
 * Does this outcome need to be shown to someone?
 *
 * `connected` and `down` are what the user asked for, and `cancelled` is what they asked for
 * instead. The rest are a cycle that ended without a tunnel, and every one of them has words in
 * the locale — including the two that used to reach nobody: `exhausted` for a reconnect that ran
 * out of protocols, and `unwind_failed` for a teardown that could not be confirmed.
 */
export function needsAttention(outcome: CycleOutcome): boolean {
  switch (outcome.outcome) {
    case 'connected':
    case 'cancelled':
    case 'down':
      return false
    case 'exhausted':
    case 'lost_gave_up':
    case 'unwind_failed':
      return true
  }
}
