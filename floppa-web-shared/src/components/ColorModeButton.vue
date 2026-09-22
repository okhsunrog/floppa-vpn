<script setup lang="ts">
import { computed } from 'vue'
import { useColorMode } from '@vueuse/core'
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
const { store } = useColorMode()

const modes = ['light', 'dark', 'auto'] as const
const icons: Record<string, string> = {
  light: 'i-lucide-sun',
  dark: 'i-lucide-moon',
  auto: 'i-lucide-monitor',
}

const currentIcon = computed(() => icons[store.value] ?? icons.auto)
const currentLabel = computed(() =>
  t('nav.themeToggle', { mode: t(`nav.theme.${store.value in icons ? store.value : 'auto'}`) }),
)

function cycle() {
  const idx = modes.indexOf(store.value as (typeof modes)[number])
  store.value = modes[(idx + 1) % modes.length] ?? 'auto'
}
</script>

<template>
  <UButton
    :icon="currentIcon"
    :aria-label="currentLabel"
    color="neutral"
    variant="ghost"
    size="sm"
    @click="cycle"
  />
</template>
