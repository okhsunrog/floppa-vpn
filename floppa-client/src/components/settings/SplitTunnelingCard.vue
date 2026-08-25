<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import type { SplitMode } from '../../bindings'
import { useVpnStore } from '../../stores/vpnStore'
import { useSettingsStore } from '../../stores/settingsStore'
import { filterApps } from '../../utils/appFilter'

const { t } = useI18n()
const vpn = useVpnStore()
const settings = useSettingsStore()

const searchQuery = ref('')
const showSystemApps = ref(false)

const modeOptions = computed(() => [
  {
    label: t('settings.modeAll'),
    value: 'all' as SplitMode,
    description: t('settings.modeAllDescription'),
    icon: 'i-lucide-globe',
  },
  {
    label: t('settings.modeInclude'),
    value: 'include' as SplitMode,
    description: t('settings.modeIncludeDescription'),
    icon: 'i-lucide-shield-check',
  },
  {
    label: t('settings.modeExclude'),
    value: 'exclude' as SplitMode,
    description: t('settings.modeExcludeDescription'),
    icon: 'i-lucide-shield-off',
  },
])

onMounted(() => {
  // App list is preloaded when the connection card mounts; this is a fallback
  settings.loadApps()
})

const filteredApps = computed(() =>
  filterApps(settings.cachedApps ?? [], {
    query: searchQuery.value,
    showSystem: showSystemApps.value,
    selected: settings.selectedApps,
  }),
)

const selectedCount = computed(() => settings.selectedApps.length)

// Track whether split tunneling settings changed while VPN is connected
const splitDirty = ref(false)
const reconnecting = ref(false)

watch(
  [() => settings.splitMode, () => settings.selectedApps],
  () => {
    if (vpn.isConnected) splitDirty.value = true
  },
  { deep: true },
)

// Clear dirty flag when VPN disconnects
watch(
  () => vpn.isConnected,
  (connected) => {
    if (!connected) splitDirty.value = false
  },
)

async function reconnectVpn() {
  reconnecting.value = true
  splitDirty.value = false
  try {
    // Re-asking with the new split rules is enough: the actor sees a tunnel that no longer
    // satisfies the request and rebuilds it.
    await vpn.connect()
  } finally {
    reconnecting.value = false
  }
}

function selectMode(mode: SplitMode) {
  settings.splitMode = mode
}
</script>

<template>
  <UCard>
    <template #header>
      <div class="flex items-center gap-2">
        <UIcon name="i-lucide-split" class="size-5" />
        <span class="font-semibold">{{ t('settings.splitTunneling') }}</span>
      </div>
    </template>

    <p class="text-sm text-[var(--ui-text-muted)] mb-4">
      {{ t('settings.splitTunnelingDescription') }}
    </p>

    <!-- Mode selector -->
    <div class="grid grid-cols-3 gap-2 mb-4">
      <button
        v-for="option in modeOptions"
        :key="option.value"
        class="flex flex-col items-center gap-1.5 p-3 rounded-lg border-2 transition-all text-center cursor-pointer"
        :class="
          settings.splitMode === option.value
            ? 'border-[var(--ui-primary)] bg-[var(--ui-primary)]/10'
            : 'border-[var(--ui-border)] hover:border-[var(--ui-border-hover)]'
        "
        @click="selectMode(option.value)"
      >
        <UIcon
          :name="option.icon"
          class="size-5"
          :class="
            settings.splitMode === option.value
              ? 'text-[var(--ui-primary)]'
              : 'text-[var(--ui-text-muted)]'
          "
        />
        <span class="text-sm font-medium">{{ option.label }}</span>
        <span class="text-xs text-[var(--ui-text-muted)] leading-tight">{{
          option.description
        }}</span>
      </button>
    </div>

    <UAlert
      v-if="splitDirty"
      color="warning"
      variant="soft"
      :title="t('settings.changesApplyOnReconnect')"
      class="mb-4"
    >
      <template #actions>
        <UButton
          :label="t('settings.reconnect')"
          color="warning"
          variant="outline"
          size="sm"
          :loading="reconnecting"
          @click="reconnectVpn"
        />
      </template>
    </UAlert>

    <!-- App list (shown for include/exclude modes) -->
    <template v-if="settings.splitMode !== 'all'">
      <div v-if="selectedCount > 0" class="mb-4">
        <UBadge color="primary" variant="subtle">
          {{ t('settings.selectedApps', { count: selectedCount }, selectedCount) }}
        </UBadge>
      </div>

      <UInput
        v-model="searchQuery"
        :placeholder="t('settings.searchApps')"
        icon="i-lucide-search"
        class="mb-3"
      />
      <div class="flex items-center gap-3 mb-4">
        <UButton
          :label="t('settings.showSystemApps')"
          :color="showSystemApps ? 'primary' : 'neutral'"
          variant="soft"
          size="sm"
          @click="() => void (showSystemApps = !showSystemApps)"
        />
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="ghost"
          size="sm"
          :loading="settings.appsLoading"
          @click="() => void settings.reloadApps()"
        />
      </div>

      <div v-if="settings.appsLoading" class="flex justify-center py-8">
        <div class="animate-spin i-lucide-loader-2 size-6 text-[var(--ui-primary)]" />
      </div>

      <div
        v-else-if="filteredApps.length === 0"
        class="text-center py-8 text-[var(--ui-text-muted)]"
      >
        {{ t('settings.noApps') }}
      </div>

      <div v-else class="flex flex-col gap-1 max-h-[60vh] overflow-y-auto">
        <label
          v-for="app in filteredApps"
          :key="app.package_name"
          class="flex items-center gap-3 px-3 py-2 rounded-lg cursor-pointer transition-colors hover:bg-[var(--ui-bg-elevated)]"
          :class="{
            'bg-[var(--ui-bg-elevated)]': settings.selectedApps.includes(app.package_name),
          }"
          style="content-visibility: auto; contain-intrinsic-size: 0 48px"
        >
          <UCheckbox
            :model-value="settings.selectedApps.includes(app.package_name)"
            @update:model-value="settings.toggleApp(app.package_name)"
          />
          <img
            v-if="app.icon"
            :src="`data:image/png;base64,${app.icon}`"
            :alt="app.label"
            class="size-8 rounded"
          />
          <div
            v-else
            class="size-8 rounded bg-[var(--ui-bg-elevated)] flex items-center justify-center"
          >
            <UIcon name="i-lucide-box" class="size-4 text-[var(--ui-text-muted)]" />
          </div>
          <div class="min-w-0 flex-1">
            <p class="text-sm font-medium truncate">{{ app.label }}</p>
            <p class="text-xs text-[var(--ui-text-muted)] truncate">{{ app.package_name }}</p>
          </div>
          <UBadge v-if="app.is_system" color="neutral" variant="subtle" size="xs">System</UBadge>
        </label>
      </div>
    </template>
  </UCard>
</template>
