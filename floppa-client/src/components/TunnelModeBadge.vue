<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settingsStore'
import { useTunnelOwner } from '../composables/useTunnelOwner'

// Mounted only on Linux. Uses the selected owner, not the connection phase:
// losing the service never makes this client a standalone owner.
const { t } = useI18n()
const settings = useSettingsStore()
const { owner, refresh } = useTunnelOwner()
const label = computed(() => {
  switch (owner.value?.kind) {
    case 'service':
      return t('settings.modeService')
    case 'service_unavailable':
      return t('settings.modeServiceUnavailable')
    case 'in_process':
      return t('settings.modeStandalone')
    default:
      return t('settings.serviceChecking')
  }
})
const unavailable = computed(() => owner.value?.kind === 'service_unavailable')
function refreshOwner() {
  void refresh()
  if (settings.settingsReady && !settings.settingsError)
    void settings.reloadOwnerSettings().catch(() => {})
}
onMounted(() => {
  refreshOwner()
  window.addEventListener('focus', refreshOwner)
})
onBeforeUnmount(() => window.removeEventListener('focus', refreshOwner))
</script>

<template>
  <div class="flex items-center justify-center gap-2 text-xs text-(--ui-text-muted)" role="status">
    <UIcon
      :name="owner?.kind === 'in_process' ? 'i-lucide-app-window' : 'i-lucide-server'"
      :class="['size-4 shrink-0', { 'text-(--ui-warning)': unavailable }]"
    />
    <span>{{ label }}</span>
  </div>
  <div v-if="settings.settingsError" class="text-xs text-(--ui-error)" role="alert">
    {{ settings.settingsError }}
    <UButton
      size="xs"
      variant="ghost"
      icon="i-lucide-refresh-cw"
      :label="t('settings.reloadSettings')"
      @click="settings.reloadOwnerSettings().catch(() => {})"
    />
  </div>
</template>
