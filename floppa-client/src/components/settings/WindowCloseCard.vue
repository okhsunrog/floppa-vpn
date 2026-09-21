<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useTunnelOwner } from '../../composables/useTunnelOwner'
import { useSettingsStore } from '../../stores/settingsStore'

/**
 * What the window's close button does — the desktop's only real setting.
 *
 * Also where a remembered answer is taken back: the prompt asks once and then never again, so
 * without a way to change it here, "remember my choice" would be irreversible.
 */
const { t } = useI18n()
const settings = useSettingsStore()
const { quittingDisconnects } = useTunnelOwner()

const items = computed(() => [
  { value: 'ask', label: t('settings.closeAsk') },
  { value: 'tray', label: t('settings.closeTray') },
  {
    value: 'quit',
    // "Disconnect and quit" is what it does when this app holds the tunnel, and is simply untrue
    // when the system service holds it: the app leaves and the VPN stays up. The card above says
    // which of the two is the case; this is the label that would otherwise contradict it.
    label: quittingDisconnects() ? t('settings.closeQuit') : t('settings.closeQuitServiceHolds'),
  },
])
</script>

<template>
  <UCard class="mb-4">
    <template #header>
      <div class="flex items-center gap-2">
        <UIcon name="i-lucide-app-window" class="size-5" />
        <span class="font-semibold">{{ t('settings.windowClose') }}</span>
      </div>
    </template>

    <p class="text-xs text-[var(--ui-text-muted)] mb-3">
      {{ t('settings.windowCloseDescription') }}
    </p>
    <URadioGroup v-model="settings.closeBehavior" :items="items" />
  </UCard>
</template>
