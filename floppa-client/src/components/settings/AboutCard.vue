<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useUpdateStore } from '../../stores/updateStore'

const { t } = useI18n()
const updateStore = useUpdateStore()
const appVersion = __APP_VERSION__
</script>

<template>
  <UCard class="mt-4">
    <template #header>
      <div class="flex items-center gap-2">
        <UIcon name="i-lucide-info" class="size-5" />
        <span class="font-semibold">{{ t('settings.about') }}</span>
      </div>
    </template>

    <div class="flex items-center justify-between">
      <div>
        <p class="text-sm font-medium">Floppa VPN</p>
        <p class="text-xs text-[var(--ui-text-muted)]">
          {{ t('settings.version', { version: appVersion }) }}
        </p>
      </div>
      <UButton
        :label="t('changelog.whatsNew')"
        icon="i-lucide-sparkles"
        variant="soft"
        size="sm"
        @click="updateStore.showBundledChangelog()"
      />
    </div>
  </UCard>
</template>
