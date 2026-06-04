<script setup lang="ts">
import { watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDragAndDrop } from '@formkit/drag-and-drop/vue'
import { useSettingsStore } from '../stores/settingsStore'
import { useVpnStore } from '../stores/vpnStore'

const open = defineModel<boolean>('open', { required: true })

const { t } = useI18n()
const settings = useSettingsStore()
const vpn = useVpnStore()
const toast = useToast()

// Local draggable list seeded from the persisted priority; synced back on reorder.
const [listRef, order] = useDragAndDrop<string>([...settings.protocolOrder], {
  dragHandle: '.drag-handle',
})

watch(
  order,
  (value) => {
    settings.protocolOrder = [...value]
  },
  { deep: true },
)

async function onReset() {
  await vpn.resetProtocolPreference()
  toast.add({ title: t('settings.protocolPreferenceReset'), color: 'success' })
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
          <ul ref="listRef" class="flex flex-col gap-2">
            <li
              v-for="proto in order"
              :key="proto"
              class="flex items-center gap-3 rounded-lg bg-[var(--ui-bg-elevated)] px-3 py-2.5"
            >
              <UIcon
                name="i-lucide-grip-vertical"
                class="drag-handle size-4 shrink-0 cursor-grab text-[var(--ui-text-muted)]"
              />
              <span class="text-sm font-medium">{{ t(`vpn.${proto}`) }}</span>
              <UBadge
                v-if="!vpn.availableProtocols.includes(proto)"
                color="neutral"
                variant="subtle"
                size="xs"
                class="ml-auto"
              >
                {{ t('settings.protocolUnavailable') }}
              </UBadge>
            </li>
          </ul>
        </div>

        <USeparator />

        <!-- Current protocol + reset -->
        <div class="flex items-center justify-between gap-4">
          <div>
            <p class="text-sm font-medium">{{ t('settings.currentProtocol') }}</p>
            <p class="text-sm text-[var(--ui-text-muted)]">{{ t(`vpn.${vpn.activeProtocol}`) }}</p>
          </div>
          <UButton
            :label="t('settings.resetProtocolPreference')"
            icon="i-lucide-rotate-ccw"
            color="neutral"
            variant="ghost"
            size="sm"
            :disabled="vpn.isLoading"
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
        <UButton :label="t('common.close')" color="primary" @click="open = false" />
      </div>
    </template>
  </UModal>
</template>
