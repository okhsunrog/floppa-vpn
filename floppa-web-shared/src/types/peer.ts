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
 * Connection status for client app
 */
export type ConnectionStatus =
  | 'disconnected'
  | 'connecting'
  | 'verifying_connection'
  | 'connected'
  | 'disconnecting'

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
