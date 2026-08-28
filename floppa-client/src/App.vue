<script setup lang="ts">
import { computed, watch } from 'vue'
import { useDark } from '@vueuse/core'
import { AppLayout, openExternal, useAuthStore } from 'floppa-web-shared'
import { useInvalidateQueries } from 'floppa-web-shared/composables/invalidate'
import { getMeQueryKey } from 'floppa-web-shared/client/@pinia/colada.gen'
import { useI18n } from 'vue-i18n'
import { useUpdateStore } from './stores/updateStore'
import { useVpnStore } from './stores/vpnStore'
import { commands } from './bindings'
import ChangelogModal from './components/ChangelogModal.vue'
import WindowClosePrompt from './components/WindowClosePrompt.vue'
import { useTray } from './composables/useTray'
import { accountChange } from './utils/account'

const { t } = useI18n()
const authStore = useAuthStore()
const updateStore = useUpdateStore()
const vpn = useVpnStore()
const isDark = useDark()
const invalidate = useInvalidateQueries()

// The tray, and with it the desktop's answer to "closing the window should not end the VPN".
// At app scope because a tray click has to work with no window on screen at all.
useTray()

// Sync status bar icon color with app theme on Android
watch(
  isDark,
  (dark) => {
    commands.setStatusBarStyle(dark).catch(() => {})
  },
  { immediate: true },
)

/**
 * Whose device this is, as far as the stored tunnel configs are concerned.
 *
 * Logging out used to take the tunnel down and stop there, so the previous account's private
 * keys, its VLESS URI and the autostart bundle stayed on the device: a second account on the
 * same phone that could not reach the server connected under the first account's identity
 * ("Using saved configuration"), and always-on could bring the logged-out account's tunnel back
 * with no one signed in at all. Forgetting them is the same act as logging out.
 *
 * Signing in as somebody else is the same situation reached from the other side — a deep link
 * for another account while one is already signed in — plus a cache that still answers `/me` for
 * the previous user. Both are handled here, through the store rather than around it: the store
 * is the one mirror of the tunnel, and a command issued past it leaves its `requesting`/`error`
 * describing a tunnel that is no longer what they say.
 */
watch(
  () => authStore.user?.id ?? null,
  async (id, previous) => {
    const change = accountChange(previous, id)
    if (change === 'none') return
    // The command issues its own Down and waits for the tunnel to be genuinely gone before it
    // wipes anything, so there is nothing to do before it.
    await vpn.forgetConfigs()
    // A cache that still answers `/me` for the previous user decides, among other things,
    // whether this one is thought to have a subscription.
    if (change === 'switch') await invalidate(getMeQueryKey())
  },
)

const forceUpdateOpen = computed({
  get: () => updateStore.forceUpdate !== null,
  set: () => {}, // prevent closing
})

async function openDownload(url: string) {
  await openExternal(url)
}
</script>

<template>
  <AppLayout
    :extra-nav-items="[
      { label: t('nav.settings'), icon: 'i-lucide-sliders-horizontal', to: '/settings' },
    ]"
  >
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
    <UModal v-model:open="forceUpdateOpen" :close="false" :dismissible="false">
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
    <WindowClosePrompt />
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
