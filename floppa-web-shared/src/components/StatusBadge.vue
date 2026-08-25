<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { BadgeProps } from '@nuxt/ui'
import type { PeerSyncStatus } from '../types'

const props = defineProps<{
  status: PeerSyncStatus
}>()

const { t } = useI18n()

// A Record over the union: adding a sync status without a badge is a compile error, not a
// badge that silently shows the raw enum value. Connection states have their own component.
const statusConfig: Record<PeerSyncStatus, { color: BadgeProps['color']; key: string }> = {
  active: { color: 'success', key: 'status.active' },
  pending_add: { color: 'warning', key: 'status.pending' },
  pending_remove: { color: 'error', key: 'status.removing' },
  removed: { color: 'error', key: 'status.removed' },
}

const config = computed(() => statusConfig[props.status])
</script>

<template>
  <UBadge :color="config.color" variant="subtle" :label="t(config.key)" />
</template>
