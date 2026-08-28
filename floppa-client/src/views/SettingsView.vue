<script setup lang="ts">
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useVpnStore } from '../stores/vpnStore'
import { useSettingsStore } from '../stores/settingsStore'
import ProtocolSettingsModal from '../components/ProtocolSettingsModal.vue'
import AndroidPermissionsCard from '../components/settings/AndroidPermissionsCard.vue'
import SplitTunnelingCard from '../components/settings/SplitTunnelingCard.vue'
import DiagnosticsCard from '../components/settings/DiagnosticsCard.vue'
import AboutCard from '../components/settings/AboutCard.vue'
import WindowCloseCard from '../components/settings/WindowCloseCard.vue'

const { t } = useI18n()
const vpn = useVpnStore()
const settings = useSettingsStore()

const protocolModalOpen = ref(false)

function openProtocolModal() {
  protocolModalOpen.value = true
}
</script>

<template>
  <div class="max-w-3xl mx-auto">
    <h1 class="text-2xl font-bold mb-6">{{ t('settings.title') }}</h1>

    <!-- Protocol selection (only when more than one protocol is available) -->
    <UCard v-if="vpn.availableProtocols.length > 1" class="mb-4">
      <template #header>
        <div class="flex items-center gap-2">
          <UIcon name="i-lucide-shuffle" class="size-5" />
          <span class="font-semibold">{{ t('settings.protocolSelection') }}</span>
        </div>
      </template>

      <div class="flex items-center justify-between gap-4">
        <div>
          <p class="text-sm font-medium">{{ t('settings.autoSelectProtocol') }}</p>
          <p class="text-xs text-[var(--ui-text-muted)]">
            {{ t('settings.autoSelectProtocolHint') }}
          </p>
        </div>
        <div class="flex items-center gap-2 shrink-0">
          <UButton
            :label="t('settings.configure')"
            icon="i-lucide-sliders-horizontal"
            color="neutral"
            variant="ghost"
            size="sm"
            @click="openProtocolModal"
          />
          <USwitch v-model="settings.autoSelect" />
        </div>
      </div>
    </UCard>

    <template v-if="vpn.isAndroid">
      <AndroidPermissionsCard />
      <SplitTunnelingCard />
    </template>

    <template v-else>
      <WindowCloseCard />

      <!-- Non-Android notice -->
      <UCard class="mb-4">
        <div class="flex flex-col items-center gap-2 py-4 text-center text-[var(--ui-text-muted)]">
          <UIcon name="i-lucide-split" class="text-3xl" />
          <p>{{ t('settings.androidOnly') }}</p>
        </div>
      </UCard>
    </template>

    <DiagnosticsCard />
    <AboutCard />

    <ProtocolSettingsModal v-model:open="protocolModalOpen" />
  </div>
</template>
