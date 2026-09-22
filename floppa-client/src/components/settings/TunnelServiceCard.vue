<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { commands } from '../../bindings'
import { useTunnelOwner } from '../../composables/useTunnelOwner'

/**
 * Which of the two places is holding the tunnel, and the one thing that depends on it.
 *
 * The mode itself reads as a status rather than a control, because it is not something to choose:
 * it is decided once when the app starts and kept for the run, since two actors must never both be
 * live. Only the boot switch below it is a setting, and it appears only in the mode that can honour
 * it.
 *
 * The mode is on the screen because the promises differ. With the system service, closing the window or
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

/**
 * Whether this machine brings its tunnel back after a reboot.
 *
 * `null` means there is nothing to answer — no service is holding the tunnel, so nothing outlives
 * this app to bring anything back — and the switch is hidden rather than shown doing nothing.
 *
 * Not `systemctl enable`, although that is where anyone would look for it. systemd gives polkit
 * the unit's name for starting and stopping a unit but not for enabling one, so a rule letting the
 * `floppa` group enable this one unit without an administrator's password cannot be written — only
 * one letting it enable any unit at all. Group membership is the authority everything else here
 * runs on, so the setting lives where that reaches.
 */
const onBoot = ref<boolean | null>(null)
const saving = ref(false)

onMounted(async () => {
  const result = await commands.getResumeOnBoot()
  if (result.status === 'ok') onBoot.value = result.data
  else console.warn(`[service] could not read the boot setting: ${result.error}`)
})

async function setOnBoot(enabled: boolean) {
  saving.value = true
  const result = await commands.setResumeOnBoot(enabled)
  saving.value = false
  if (result.status === 'ok') onBoot.value = enabled
  else console.error(`[service] could not change the boot setting: ${result.error}`)
}

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

    <div
      v-if="onBoot !== null"
      class="flex items-center justify-between gap-3 mt-4 pt-4 border-t border-(--ui-border)"
    >
      <div>
        <p class="text-sm font-medium">{{ t('settings.connectOnBoot') }}</p>
        <p class="text-xs text-(--ui-text-muted) mt-1">{{ t('settings.connectOnBootDetail') }}</p>
      </div>
      <USwitch
        :model-value="onBoot"
        :disabled="saving"
        :aria-label="t('settings.connectOnBoot')"
        @update:model-value="setOnBoot"
      />
    </div>
  </UCard>
</template>
