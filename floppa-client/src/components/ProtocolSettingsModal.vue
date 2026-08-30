<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useVpnStore } from '../stores/vpnStore'
import ProtocolPriorityList from './ProtocolPriorityList.vue'

const open = defineModel<boolean>('open', { required: true })

const { t } = useI18n()
const vpn = useVpnStore()
const toast = useToast()

async function onReset() {
  if (await vpn.forgetPreferred()) {
    toast.add({ title: t('settings.protocolPreferenceReset'), color: 'success' })
  } else {
    toast.add({ title: t('settings.protocolPreferenceResetFailed'), color: 'error' })
  }
}

function closeModal() {
  open.value = false
}
</script>

<template>
  <UModal v-model:open="open" :title="t('settings.protocolSettings')">
    <template #body>
      <div class="flex flex-col gap-5">
        <!-- Priority order (drag to reorder) -->
        <div>
          <p class="text-sm font-medium mb-1">{{ t('settings.protocolPriority') }}</p>
          <p class="text-xs text-[var(--ui-text-muted)] mb-3">
            {{ t('settings.protocolPriorityHint') }}
          </p>
          <ProtocolPriorityList />
        </div>

        <USeparator />

        <!-- Current protocol + reset -->
        <div class="flex items-center justify-between gap-4">
          <div>
            <p class="text-sm font-medium">{{ t('settings.currentProtocol') }}</p>
            <!-- `activeProtocol` is undefined until something is stored; `vpn.undefined` is
                 not a translation key, and rendered as one it showed up verbatim. -->
            <p class="text-sm text-[var(--ui-text-muted)]">
              {{
                vpn.activeProtocol ? t(`vpn.${vpn.activeProtocol}`) : t('settings.noProtocolYet')
              }}
            </p>
          </div>
          <UButton
            :label="t('settings.resetProtocolPreference')"
            icon="i-lucide-rotate-ccw"
            color="neutral"
            variant="ghost"
            size="sm"
            :disabled="vpn.isBusy"
            @click="onReset"
          />
        </div>

        <USeparator />

        <!-- How it works -->
        <p class="text-xs leading-relaxed text-[var(--ui-text-muted)]">
          {{ t('settings.autoSelectHelp') }}
        </p>
      </div>
    </template>

    <template #footer>
      <div class="flex justify-end">
        <UButton :label="t('common.close')" color="primary" @click="closeModal" />
      </div>
    </template>
  </UModal>
</template>
