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
        <UIcon name="i-lucide-network" class="size-5" />
        <span class="font-semibold">{{ t('settings.localNetwork') }}</span>
      </div>
    </template>

    <div class="flex items-center justify-between gap-4">
      <div>
        <p class="text-sm font-medium">{{ t('settings.allowLocalNetwork') }}</p>
        <p class="text-xs text-(--ui-text-muted) mt-1">
          {{ t('settings.allowLocalNetworkHint') }}
        </p>
      </div>
      <USwitch
        v-model="settings.allowLan"
        :aria-label="t('settings.allowLocalNetwork')"
        class="shrink-0"
      />
    </div>
  </UCard>
</template>
