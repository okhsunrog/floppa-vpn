<script setup lang="ts">
import { computed, ref } from 'vue'
import { platform } from '@tauri-apps/plugin-os'
import { useI18n } from 'vue-i18n'
import type { RouteRule, RouteTarget } from '../../bindings'
import { useSettingsStore } from '../../stores/settingsStore'
import { useVpnStore } from '../../stores/vpnStore'
import { normalizeDestination, sortDestinationRules } from '../../utils/destinationRules'

const { t } = useI18n()
const settings = useSettingsStore()
const vpn = useVpnStore()
const name = ref('')
const destination = ref('')
const target = ref<RouteTarget>('direct')
const error = ref('')
const targets = computed(() =>
  ['direct', 'tunnel', 'system'].map((value) => ({ label: t(`routing.${value}`), value })),
)

function add() {
  const cidr = normalizeDestination(destination.value)
  if (!cidr) {
    error.value = t('routing.invalid')
    return
  }
  if (
    settings.destinationRules.length >= 64 ||
    settings.destinationRules.some((r) => r.destination === cidr)
  ) {
    error.value = t('routing.duplicate')
    return
  }
  const rule: RouteRule = { name: name.value.trim(), destination: cidr, target: target.value }
  settings.destinationRules = sortDestinationRules([...settings.destinationRules, rule])
  destination.value = ''
  name.value = ''
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
      v-for="(rule, index) in settings.destinationRules"
      :key="rule.destination"
      class="flex items-center justify-between gap-3 py-2 border-b border-(--ui-border)"
    >
      <div>
        <p class="font-medium">{{ rule.name || rule.destination }}</p>
        <p class="text-sm text-(--ui-text-muted)">
          {{ rule.destination }} · {{ t(`routing.${rule.target}`) }}
        </p>
      </div>
      <UButton
        color="neutral"
        variant="ghost"
        :label="t('routing.remove')"
        @click="settings.destinationRules.splice(index, 1)"
      />
    </div>
    <form class="grid gap-3 mt-4" @submit.prevent="add">
      <UInput v-model="name" :placeholder="t('routing.name')" :aria-label="t('routing.name')" />
      <UInput
        v-model="destination"
        placeholder="194.186.63.0/24"
        :aria-label="t('routing.destination')"
      />
      <USelect v-model="target" :items="targets" :aria-label="t('routing.target')" />
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
