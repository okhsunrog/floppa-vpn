<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { PeerSyncStatus, ConnectionStatus } from '../types'

const props = defineProps<{
  status: PeerSyncStatus | ConnectionStatus
}>()

const { t } = useI18n()

const statusConfig: Record<string, { color: string; key: string }> = {
  // Peer sync statuses
  active: { color: 'success', key: 'status.active' },
  pending_add: { color: 'warning', key: 'status.pending' },
  pending_remove: { color: 'error', key: 'status.removing' },
  removed: { color: 'error', key: 'status.removed' },
  // Connection statuses
  connected: { color: 'success', key: 'status.connected' },
  connecting: { color: 'warning', key: 'status.connecting' },
  verifying_connection: { color: 'warning', key: 'status.verifyingConnection' },
  disconnected: { color: 'neutral', key: 'status.disconnected' },
  disconnecting: { color: 'warning', key: 'status.disconnecting' },
}

const config = computed(() => {
  const cfg = statusConfig[props.status]
  return cfg ? { color: cfg.color, label: t(cfg.key) } : { color: 'neutral', label: props.status }
})
</script>

<template>
  <UBadge :color="config.color as any" variant="subtle" :label="config.label" />
</template>
