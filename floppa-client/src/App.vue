<script setup lang="ts">
import { computed, watch } from 'vue'
import { useDark } from '@vueuse/core'
import { AppLayout, useAuthStore } from 'floppa-web-shared'
import { useI18n } from 'vue-i18n'
import { useUpdateStore } from './stores/updateStore'
import { openUrl } from '@tauri-apps/plugin-opener'
import { commands } from './bindings'
import ChangelogModal from './components/ChangelogModal.vue'

const { t } = useI18n()
const authStore = useAuthStore()
const updateStore = useUpdateStore()
const isDark = useDark()

// Sync status bar icon color with app theme on Android
watch(isDark, (dark) => {
  commands.setStatusBarStyle(dark)
}, { immediate: true })

// Stop VPN tunnel on logout
watch(() => authStore.isAuthenticated, (authenticated, wasAuthenticated) => {
  if (wasAuthenticated && !authenticated) {
    commands.disconnect()
  }
})

const forceUpdateOpen = computed({
  get: () => updateStore.forceUpdate !== null,
  set: () => {}, // prevent closing
})

async function openDownload(url: string) {
  await openUrl(url)
}
</script>

<template>
  <AppLayout :extra-nav-items="[{ label: t('nav.settings'), icon: 'i-lucide-sliders-horizontal', to: '/settings' }]">
    <!-- Voluntary update banner -->
    <div
      v-if="updateStore.updateInfo && !updateStore.dismissed && !updateStore.forceUpdate"
      class="flex flex-wrap items-center gap-2 rounded-lg bg-(--ui-bg-elevated) p-3 mb-4"
    >
      <div class="flex items-center gap-2 shrink-0">
        <UIcon name="i-lucide-download" class="text-(--ui-primary) shrink-0" />
        <span class="text-sm whitespace-nowrap">
          {{ t('update.available', { version: updateStore.updateInfo.version }) }}
        </span>
      </div>
      <div class="flex gap-2 shrink-0">
        <UButton size="xs" variant="ghost" color="neutral" @click="updateStore.dismiss()">
          {{ t('update.dismiss') }}
        </UButton>
        <UButton size="xs" variant="soft" @click="updateStore.openChangelogForUpdate()">
          {{ t('changelog.whatsNew') }}
        </UButton>
        <UButton size="xs" @click="openDownload(updateStore.updateInfo.downloadUrl)">
          {{ t('update.download') }}
        </UButton>
      </div>
    </div>

    <!-- Forced update overlay -->
    <UModal v-model:open="forceUpdateOpen" :closeable="false" :dismissible="false">
      <template #content>
        <div class="p-6 text-center space-y-4">
          <UIcon name="i-lucide-alert-triangle" class="text-(--ui-warning) size-12 mx-auto" />
          <h2 class="text-lg font-semibold">{{ t('update.required') }}</h2>
          <p class="text-sm text-(--ui-text-muted)">
            {{ t('update.requiredDescription', { version: updateStore.forceUpdate?.minVersion }) }}
          </p>
          <UButton
            v-if="updateStore.updateInfo"
            block
            @click="openDownload(updateStore.updateInfo.downloadUrl)"
          >
            {{ t('update.download') }}
          </UButton>
          <UButton v-else block @click="updateStore.checkForUpdates()">
            {{ t('update.checkNow') }}
          </UButton>
        </div>
      </template>
    </UModal>

    <ChangelogModal />
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
