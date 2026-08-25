<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { InfoView } from 'floppa-web-shared/views'

const { t } = useI18n()
const appVersion = __APP_VERSION__
</script>

<template>
  <InfoView>
    <template #app-info>
      <span class="text-sm text-[var(--ui-text-muted)]">
        {{ t('info.version', { version: appVersion }) }}
      </span>
    </template>
  </InfoView>
</template>
