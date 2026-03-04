<script setup lang="ts">
import { computed } from 'vue'
import { useColorMode } from '@vueuse/core'

const { store } = useColorMode()

const modes = ['light', 'dark', 'auto'] as const
const icons: Record<string, string> = {
  light: 'i-lucide-sun',
  dark: 'i-lucide-moon',
  auto: 'i-lucide-monitor',
}

const currentIcon = computed(() => icons[store.value] ?? icons.auto)

function cycle() {
  const idx = modes.indexOf(store.value as (typeof modes)[number])
  store.value = modes[(idx + 1) % modes.length] ?? 'auto'
}
</script>

<template>
  <UButton :icon="currentIcon" color="neutral" variant="ghost" size="sm" @click="cycle" />
</template>
