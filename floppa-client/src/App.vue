<script setup lang="ts">
import { AppLayout } from 'floppa-web-shared'
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
</script>

<template>
  <AppLayout :extra-nav-items="[{ label: t('nav.settings'), icon: 'i-lucide-sliders-horizontal', to: '/settings' }]">
    <RouterView />
  </AppLayout>
</template>

<style>
html,
body {
  margin: 0;
  padding: 0;
  min-height: 100vh;
}

body {
  background: var(--ui-bg);
  color: var(--ui-text-highlighted);
}
</style>
