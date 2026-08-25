/** Peer sync status — the server's enum, re-exported from the generated client. */
export type { PeerSyncStatus } from '../client/types.gen'

/**
 * Connection status for client app.
 *
 * Mirrors the `Phase` the Rust actor publishes. `retrying` is the one addition: waiting out a
 * backoff before the next attempt is still work in progress, and showing it as "disconnected"
 * would make an actively-reconnecting tunnel look dead.
 */
export type ConnectionStatus =
  /** Nothing has been observed yet. Not a claim that there is no tunnel — see `Phase::Unknown`. */
  | 'unknown'
  | 'disconnected'
  | 'connecting'
  | 'verifying_connection'
  | 'connected'
  | 'disconnecting'
  | 'retrying'
