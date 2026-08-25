import type { CycleOutcome } from '../bindings'

/** The last unsolicited outcome acted on: which cycle it belonged to, and how it ended. */
export interface HandledOutcome {
  epoch: number
  outcome: CycleOutcome['outcome']
}

/**
 * Whether an outcome published for `epoch` is one nobody has acted on yet.
 *
 * The actor republishes the whole state on every tick, so the same outcome arrives many times
 * and has to be deduplicated — but by `{ epoch, outcome }`, not by epoch alone. A cycle that
 * connects and later loses the tunnel for good reports `connected` and then `lost_gave_up`
 * under one epoch (the intent never changed), and keying on the epoch swallowed the second.
 */
export function isUnhandledOutcome(
  handled: HandledOutcome | null,
  epoch: number,
  outcome: CycleOutcome,
): boolean {
  return handled === null || handled.epoch !== epoch || handled.outcome !== outcome.outcome
}
