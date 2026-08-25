import type { CycleOutcome } from '../bindings'

/** The last outcome acted on: which cycle it was, and — for a human reading a log — how it ended. */
export interface HandledOutcome {
  /** `TunnelState.outcome_serial`: the one thing that identifies a cycle's outcome. */
  serial: number
  outcome: CycleOutcome['outcome']
}

/**
 * Whether the published outcome is one nobody has acted on yet.
 *
 * The actor republishes the whole state on every tick and `last_outcome` stays put until the next
 * accepted intent, so the same outcome arrives many times and has to be deduplicated. What it is
 * deduplicated *by* has been wrong twice. By epoch alone: a cycle that connects and later loses
 * the tunnel for good reports `connected` and then `lost_gave_up` under one epoch, and the second
 * was swallowed. By `{ epoch, outcome }`: a *reconnect* runs under the same intent, so a tunnel
 * that dropped and came back reports `connected` twice under one epoch — and the second one is
 * precisely the one carrying "a protocol was stepped over and its peer needs repairing". On the
 * device that meant a dead AmneziaWG peer was never recreated.
 *
 * So the actor stamps every outcome with a serial, and that is the whole key.
 *
 * This lives beside the store rather than in a component: the record used to be a local of
 * `VpnCard`, so returning to the dashboard re-handled a `lost_gave_up` from minutes ago — a peer
 * lookup on every mount, and a connect nobody asked for if the peer really was gone.
 */
export function isUnhandledOutcome(handled: HandledOutcome | null, serial: number): boolean {
  return handled === null || handled.serial !== serial
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
