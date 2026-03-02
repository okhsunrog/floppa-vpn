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
  connected: 'status.connected',
  connecting: 'status.connecting',
  disconnected: 'status.disconnected',
  disconnecting: 'status.disconnecting',
  error: 'status.connectionError',
}

const dotClass = computed(() => {
  const classes: Record<ConnectionStatus, string> = {
    connected: 'bg-green-500',
    connecting: 'bg-yellow-500',
    disconnected: 'bg-neutral-400',
    disconnecting: 'bg-yellow-500',
    error: 'bg-red-500',
  }
  return classes[props.status]
})

const isPulsing = computed(() => props.status === 'connecting' || props.status === 'disconnecting')
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
