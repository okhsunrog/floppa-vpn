<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import type { LogProfile } from '../../bindings'
import { useLogConfig, type LogNotice } from '../../composables/useLogConfig'

const { t } = useI18n()
const toast = useToast()

const showAdvanced = ref(false)

const profileOptions = [
  { label: 'Normal', value: 'normal' as LogProfile },
  { label: 'Verbose', value: 'verbose' as LogProfile },
]

/** The one place a log-config event becomes a toast. */
function notify(notice: LogNotice) {
  switch (notice.kind) {
    case 'load_failed':
      toast.add({
        title: t('settings.logConfigLoadFailed'),
        description: notice.detail,
        color: 'error',
      })
      break
    case 'save_failed':
      toast.add({ title: t('settings.logConfigSaveFailed'), color: 'error' })
      break
    case 'capture_failed':
      toast.add({
        title: t('settings.logCaptureFailed'),
        description: notice.detail,
        color: 'error',
      })
      break
    case 'export_failed':
      toast.add({ title: t('settings.logsExportFailed'), color: 'error' })
      break
    case 'exported':
      toast.add({ title: t('settings.logsExported'), color: 'success' })
      break
  }
}

const {
  logConfig,
  captureStatus,
  customFilterInput,
  saving,
  captureBusy,
  exporting,
  load,
  setProfile,
  applyCustomFilter,
  setCustomFilterEnabled,
  clearCustomFilter,
  toggleCapture,
  exportLogs,
} = useLogConfig(notify)

onMounted(load)
</script>

<template>
  <UCard class="mt-4">
    <template #header>
      <div class="flex items-center gap-2">
        <UIcon name="i-lucide-stethoscope" class="size-5" />
        <span class="font-semibold">{{ t('settings.diagnostics') }}</span>
      </div>
    </template>

    <p class="text-sm text-[var(--ui-text-muted)] mb-4">
      {{ t('settings.diagnosticsDescription') }}
    </p>

    <div class="flex flex-col gap-4">
      <div class="flex items-center justify-between gap-4">
        <div>
          <p class="text-sm font-medium">{{ t('settings.logProfile') }}</p>
          <p class="text-xs text-[var(--ui-text-muted)]">
            {{ t('settings.logProfileDescription') }}
          </p>
        </div>
        <USelect
          :model-value="logConfig.profile"
          :items="profileOptions"
          value-key="value"
          class="w-32 shrink-0"
          size="sm"
          :disabled="saving || captureStatus.active"
          @update:model-value="(v: string) => setProfile(v as LogProfile)"
        />
      </div>

      <USeparator class="my-1" />

      <button
        class="flex items-center gap-2 text-sm text-[var(--ui-text-muted)] cursor-pointer"
        @click="showAdvanced = !showAdvanced"
      >
        <UIcon
          :name="showAdvanced ? 'i-lucide-chevron-down' : 'i-lucide-chevron-right'"
          class="size-4"
        />
        {{ t('settings.advancedLogFilter') }}
      </button>

      <div v-if="showAdvanced" class="flex flex-col gap-3">
        <p class="text-xs text-[var(--ui-text-muted)]">
          {{ t('settings.advancedLogFilterDescription') }}
        </p>
        <USwitch
          :model-value="logConfig.custom_filter_enabled"
          :label="t('settings.customFilterEnabled')"
          :disabled="!logConfig.custom_filter || saving || captureStatus.active"
          @update:model-value="(v: boolean) => setCustomFilterEnabled(v)"
        />
        <div class="flex gap-2">
          <UInput
            v-model="customFilterInput"
            :placeholder="t('settings.customFilterPlaceholder')"
            class="flex-1"
            size="sm"
          />
          <UButton
            :label="t('settings.apply')"
            size="sm"
            :loading="saving"
            :disabled="captureStatus.active"
            @click="applyCustomFilter"
          />
          <UButton
            v-if="logConfig.custom_filter"
            icon="i-lucide-x"
            size="sm"
            variant="ghost"
            :disabled="captureStatus.active"
            @click="clearCustomFilter"
          />
        </div>
        <UAlert
          v-if="logConfig.custom_filter_enabled"
          color="info"
          variant="soft"
          :title="t('settings.customFilterActive')"
          class="text-xs"
        />
      </div>

      <USeparator class="my-1" />

      <div class="flex items-center justify-between gap-4">
        <div>
          <p class="text-sm font-medium">{{ t('settings.logCapture') }}</p>
          <p class="text-xs text-[var(--ui-text-muted)]">
            {{
              captureStatus.active
                ? t('settings.logCaptureActive', { id: captureStatus.capture_id })
                : t('settings.logCaptureDescription')
            }}
          </p>
        </div>
        <UButton
          :label="
            captureStatus.active ? t('settings.stopLogCapture') : t('settings.startLogCapture')
          "
          :icon="captureStatus.active ? 'i-lucide-square' : 'i-lucide-circle-dot'"
          :color="captureStatus.active ? 'error' : 'primary'"
          variant="soft"
          size="sm"
          :loading="captureBusy"
          @click="toggleCapture"
        />
      </div>

      <div class="flex items-center justify-between gap-4">
        <div>
          <p class="text-sm font-medium">{{ t('settings.exportLatestCapture') }}</p>
          <p class="text-xs text-[var(--ui-text-muted)]">
            {{ t('settings.exportLatestCaptureDescription') }}
          </p>
        </div>
        <UButton
          :label="t('settings.exportLogs')"
          icon="i-lucide-share"
          variant="soft"
          size="sm"
          :loading="exporting"
          :disabled="captureStatus.active || !captureStatus.capture_id"
          @click="exportLogs"
        />
      </div>
    </div>
  </UCard>
</template>
