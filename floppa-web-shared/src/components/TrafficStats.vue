<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { formatBytes, formatSpeed } from '../utils'

defineProps<{
  txBytes: number
  rxBytes: number
  txSpeed?: number
  rxSpeed?: number
  showSpeed?: boolean
}>()

const { t } = useI18n()
</script>

<template>
  <div class="flex gap-6">
    <div class="flex items-center gap-2">
      <UIcon name="i-lucide-arrow-up" class="text-xl font-bold text-green-500" />
      <div class="flex flex-col">
        <span class="font-semibold">{{ formatBytes(txBytes) }}</span>
        <span v-if="showSpeed && txSpeed !== undefined" class="text-xs text-[var(--ui-text-muted)]">
          {{ formatSpeed(txSpeed) }}
        </span>
      </div>
      <span class="text-xs text-[var(--ui-text-muted)] uppercase">{{ t('traffic.tx') }}</span>
    </div>
    <div class="flex items-center gap-2">
      <UIcon name="i-lucide-arrow-down" class="text-xl font-bold text-[var(--ui-primary)]" />
      <div class="flex flex-col">
        <span class="font-semibold">{{ formatBytes(rxBytes) }}</span>
        <span v-if="showSpeed && rxSpeed !== undefined" class="text-xs text-[var(--ui-text-muted)]">
          {{ formatSpeed(rxSpeed) }}
        </span>
      </div>
      <span class="text-xs text-[var(--ui-text-muted)] uppercase">{{ t('traffic.rx') }}</span>
    </div>
  </div>
</template>
