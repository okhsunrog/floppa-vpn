<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useTunnelOwner } from '../../composables/useTunnelOwner'

/**
 * Which of the two places is holding the tunnel, said out loud.
 *
 * It is not a setting and there is nothing to click, which is why it reads as a status rather than
 * a control — the mode is decided once when the app starts and kept for the run, because two
 * actors must never both be live.
 *
 * It is on the screen because the promises differ. With the system service, closing the window or
 * quitting leaves the tunnel up; without it, quitting takes the tunnel down. Nothing else in the
 * app distinguishes those, so without this the same words would mean two different things and
 * nobody could tell which they had.
 *
 * The third state is the one worth the card on its own: a service is installed and running, and
 * this user is not in the group that may use it. Everything still works — the app falls back to
 * running the tunnel itself — so nothing looks wrong, and without being told, nobody would ever
 * discover why they are asked for a password on every connect.
 */
const { t } = useI18n()
// Calling it is what asks; the answer arrives a tick later and the card re-renders.
const { owner, lockedOut } = useTunnelOwner()

const state = computed(() => {
  if (owner.value?.kind === 'service') {
    return {
      icon: 'i-lucide-shield-check',
      tone: 'text-(--ui-success)',
      title: t('settings.serviceActive'),
      detail: t('settings.serviceActiveDetail'),
    }
  }
  if (lockedOut()) {
    return {
      icon: 'i-lucide-shield-alert',
      tone: 'text-(--ui-warning)',
      title: t('settings.serviceLockedOut'),
      detail: t('settings.serviceLockedOutDetail'),
    }
  }
  return {
    icon: 'i-lucide-app-window',
    tone: 'text-(--ui-text-muted)',
    title: t('settings.serviceAbsent'),
    detail: t('settings.serviceAbsentDetail'),
  }
})
</script>

<template>
  <UCard class="mb-4">
    <template #header>
      <div class="flex items-center gap-2">
        <UIcon name="i-lucide-server" class="size-5" />
        <span class="font-semibold">{{ t('settings.tunnelService') }}</span>
      </div>
    </template>

    <div class="flex items-start gap-3">
      <UIcon :name="state.icon" :class="['size-5 shrink-0 mt-0.5', state.tone]" />
      <div>
        <p class="text-sm font-medium">{{ state.title }}</p>
        <p class="text-xs text-(--ui-text-muted) mt-1">{{ state.detail }}</p>
      </div>
    </div>
  </UCard>
</template>
