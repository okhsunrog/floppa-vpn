<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ConnectionStatus } from '../types'

const props = defineProps<{
  status: ConnectionStatus
  showLabel?: boolean
}>()

const { t } = useI18n()

const statusKeys: Record<ConnectionStatus, string> = {
  unknown: 'status.unknown',
  connected: 'status.connected',
  connecting: 'status.connecting',
  verifying_connection: 'status.verifyingConnection',
  disconnected: 'status.disconnected',
  disconnecting: 'status.disconnecting',
  retrying: 'status.retrying',
}

const dotClass = computed(() => {
  const classes: Record<ConnectionStatus, string> = {
    // Neutral, not red: we are not reporting a problem, we are reporting that we do not know yet.
    unknown: 'bg-neutral-400',
    connected: 'bg-green-500',
    connecting: 'bg-yellow-500',
    verifying_connection: 'bg-yellow-500',
    disconnected: 'bg-neutral-400',
    disconnecting: 'bg-yellow-500',
    retrying: 'bg-yellow-500',
  }
  return classes[props.status]
})

const isPulsing = computed(
  () =>
    props.status === 'unknown' ||
    props.status === 'connecting' ||
    props.status === 'verifying_connection' ||
    props.status === 'disconnecting' ||
    props.status === 'retrying',
)
</script>

<template>
  <div class="inline-flex items-center gap-2">
    <span
      class="size-2.5 rounded-full shrink-0"
      :class="[dotClass, isPulsing && 'animate-pulse']"
    />
    <span v-if="showLabel" class="text-sm font-medium">{{ t(statusKeys[status]) }}</span>
  </div>
</template>
