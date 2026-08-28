<script setup lang="ts">
/**
 * A password field that can show what was typed.
 *
 * Nuxt UI has no built-in toggle — the documented pattern is a button in the `#trailing` slot
 * that swaps `type` and the icon — so this wraps it once instead of leaving three copies to
 * drift. Every password field in the app is this component.
 *
 * `autocomplete` is required rather than defaulted: the browser needs `current-password` on a
 * sign-in field and `new-password` where one is being set, and guessing wrong is what makes a
 * password manager offer the wrong thing.
 */
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'

defineProps<{
  placeholder?: string
  /** `current-password` when signing in, `new-password` when setting one. */
  autocomplete: 'current-password' | 'new-password'
}>()

const model = defineModel<string>({ required: true })

const { t } = useI18n()
const revealed = ref(false)
</script>

<template>
  <UInput
    v-model="model"
    :type="revealed ? 'text' : 'password'"
    :placeholder="placeholder"
    :autocomplete="autocomplete"
    icon="i-lucide-lock"
    :ui="{ trailing: 'pe-1' }"
  >
    <template #trailing>
      <!--
        `tabindex="-1"`: tabbing from the password field should reach the submit button, not a
        control that only changes how the field looks. It stays reachable by pointer, and by
        keyboard users who want it, without standing in the way of the form.
      -->
      <UButton
        color="neutral"
        variant="link"
        size="sm"
        tabindex="-1"
        :icon="revealed ? 'i-lucide-eye-off' : 'i-lucide-eye'"
        :aria-label="revealed ? t('common.hidePassword') : t('common.showPassword')"
        :aria-pressed="revealed"
        @click="revealed = !revealed"
      />
    </template>
  </UInput>
</template>
