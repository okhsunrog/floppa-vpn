<script setup lang="ts">
import { ref } from 'vue'
import { platform } from '@tauri-apps/plugin-os'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settingsStore'
import { useVpnStore } from '../../stores/vpnStore'
import { normalizeDestination } from '../../utils/routeExclusions'

const { t } = useI18n()
const settings = useSettingsStore()
const vpn = useVpnStore()
const destination = ref('')
const error = ref('')
function add() {
  const cidr = normalizeDestination(destination.value)
  if (!cidr) {
    error.value = t('routing.invalid')
    return
  }
  if (settings.excludedRoutes.length >= 64 || settings.excludedRoutes.includes(cidr)) {
    error.value = t('routing.duplicate')
    return
  }
  settings.excludedRoutes = [...settings.excludedRoutes, cidr].sort()
  destination.value = ''
  error.value = ''
}
</script>
<template>
  <UCard v-if="platform() === 'linux'" class="mb-4">
    <template #header
      ><div class="font-semibold">{{ t('routing.title') }}</div></template
    >
    <p class="text-sm text-(--ui-text-muted) mb-4">{{ t('routing.description') }}</p>
    <div
      v-for="(route, index) in settings.excludedRoutes"
      :key="route"
      class="flex items-center justify-between gap-3 py-2 border-b border-(--ui-border)"
    >
      <span>{{ route }}</span>
      <UButton
        color="neutral"
        variant="ghost"
        :label="t('routing.remove')"
        @click="settings.excludedRoutes.splice(index, 1)"
      />
    </div>
    <form class="grid gap-3 mt-4" @submit.prevent="add">
      <UInput
        v-model="destination"
        placeholder="194.186.63.0/24"
        :aria-label="t('routing.destination')"
      />
      <p v-if="error" role="alert" class="text-error text-sm">{{ error }}</p>
      <UButton type="submit" :label="t('routing.add')" class="justify-center" />
    </form>
    <p class="text-xs text-(--ui-text-muted) mt-3">{{ t('routing.hint') }}</p>
    <UButton
      v-if="vpn.splitDirty"
      class="mt-3"
      :disabled="vpn.isBusy"
      :label="t('routing.apply')"
      @click="vpn.connect()"
    />
  </UCard>
</template>
