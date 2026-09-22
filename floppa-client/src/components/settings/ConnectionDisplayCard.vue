<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settingsStore'

const { t } = useI18n()
const settings = useSettingsStore()
</script>

<template>
  <UCard class="mb-4">
    <template #header>
      <div class="flex items-center gap-2">
        <UIcon name="i-lucide-monitor-cog" class="size-5" />
        <span class="font-semibold">{{ t('settings.connectionDisplay') }}</span>
      </div>
    </template>

    <div class="flex items-center justify-between gap-4">
      <div>
        <p class="text-sm font-medium">{{ t('settings.showConnectionDetails') }}</p>
        <p class="text-xs text-(--ui-text-muted) mt-1">
          {{ t('settings.showConnectionDetailsHint') }}
        </p>
      </div>
      <USwitch
        v-model="settings.showConnectionDetails"
        :aria-label="t('settings.showConnectionDetails')"
        class="shrink-0"
      />
    </div>
  </UCard>
</template>
