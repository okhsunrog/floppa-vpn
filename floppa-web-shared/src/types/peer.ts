/**
 * Peer sync status
 */
export type PeerSyncStatus = 'pending_add' | 'active' | 'pending_remove' | 'removed'

/**
 * Basic peer information shared between web and client
 */
export interface PeerInfo {
  id: number
  assignedIp: string
  syncStatus: PeerSyncStatus
  txBytes: number
  rxBytes: number
  lastHandshake: string | null
}

/**
 * Connection status for client app.
 *
 * Mirrors the `Phase` the Rust actor publishes. `retrying` is the one addition: waiting out a
 * backoff before the next attempt is still work in progress, and showing it as "disconnected"
 * would make an actively-reconnecting tunnel look dead.
 */
export type ConnectionStatus =
  | 'disconnected'
  | 'connecting'
  | 'verifying_connection'
  | 'connected'
  | 'disconnecting'
  | 'retrying'

/**
 * Real-time connection stats for client app
 */
export interface ConnectionStats {
  status: ConnectionStatus
  connectedAt: Date | null
  serverEndpoint: string | null
  assignedIp: string | null
  txBytes: number
  rxBytes: number
  txBytesPerSec: number
  rxBytesPerSec: number
  lastHandshake: Date | null
}
