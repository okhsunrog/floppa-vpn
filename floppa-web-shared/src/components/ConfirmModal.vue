<script setup lang="ts">
import { useI18n } from 'vue-i18n'

/**
 * A yes/no dialog for one row action. The wording is the caller's: `message` is already built
 * (it usually names the row), and `confirmLabel` says what the button does.
 */
withDefaults(
  defineProps<{
    title: string
    message: string
    confirmLabel: string
    confirmColor?: 'error' | 'warning' | 'primary'
    loading?: boolean
  }>(),
  { confirmColor: 'error', loading: false },
)

const open = defineModel<boolean>('open', { required: true })

const emit = defineEmits<{ confirm: [] }>()

const { t } = useI18n()
</script>

<template>
  <UModal v-model:open="open" :title="title">
    <template #body>
      <p>{{ message }}</p>
    </template>
    <template #footer>
      <UButton
        :label="t('common.cancel')"
        color="neutral"
        variant="outline"
        @click="() => void (open = false)"
      />
      <UButton
        :label="confirmLabel"
        :color="confirmColor"
        :loading="loading"
        @click="() => emit('confirm')"
      />
    </template>
  </UModal>
</template>
