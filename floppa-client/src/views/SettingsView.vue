<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useVpnStore } from '../stores/vpnStore'
import { useSettingsStore, type SplitMode } from '../stores/settingsStore'
import { useUpdateStore } from '../stores/updateStore'
import { useAndroidPermissions } from '../composables/useAndroidPermissions'

const { t } = useI18n()
const vpn = useVpnStore()
const settings = useSettingsStore()
const updateStore = useUpdateStore()
const permissions = useAndroidPermissions()
const appVersion = __APP_VERSION__

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
  if (vpn.isAndroid) {
    permissions.checkBatteryOptimization()
    permissions.checkNotifications()
    // App list is preloaded at startup (VpnCard); this is a fallback
    settings.loadApps()
  }
})

const filteredApps = computed(() => {
  let list = settings.cachedApps ?? []

  if (!showSystemApps.value) {
    list = list.filter((a) => !a.is_system)
  }

  if (searchQuery.value) {
    const q = searchQuery.value.toLowerCase().replace(/\s+/g, '')
    list = list.filter(
      (a) =>
        a.label.toLowerCase().replace(/\s+/g, '').includes(q) ||
        a.package_name.toLowerCase().includes(q),
    )
  }

  // Selected apps first, then alphabetical
  return [...list].sort((a, b) => {
    const aSelected = settings.selectedApps.includes(a.package_name)
    const bSelected = settings.selectedApps.includes(b.package_name)
    if (aSelected !== bSelected) return aSelected ? -1 : 1
    return a.label.localeCompare(b.label)
  })
})

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
    await vpn.reconnect()
  } finally {
    reconnecting.value = false
  }
}

function selectMode(mode: SplitMode) {
  settings.splitMode = mode
}
</script>

<template>
  <div class="max-w-3xl mx-auto">
    <h1 class="text-2xl font-bold mb-6">{{ t('settings.title') }}</h1>

    <!-- Notifications (Android only) -->
    <UCard v-if="vpn.isAndroid && permissions.notificationsEnabled.value !== null" class="mb-4">
      <template #header>
        <div class="flex items-center gap-2">
          <UIcon name="i-lucide-bell" class="size-5" />
          <span class="font-semibold">{{ t('settings.notifications') }}</span>
        </div>
      </template>

      <p class="text-sm text-[var(--ui-text-muted)] mb-4">
        {{ t('settings.notificationsDescription') }}
      </p>

      <div class="flex items-center justify-between">
        <div class="flex items-center gap-2">
          <UIcon
            :name="
              permissions.notificationsEnabled.value
                ? 'i-lucide-check-circle'
                : 'i-lucide-alert-triangle'
            "
            :class="permissions.notificationsEnabled.value ? 'text-green-500' : 'text-yellow-500'"
            class="size-5"
          />
          <span class="text-sm">
            {{
              permissions.notificationsEnabled.value
                ? t('settings.notificationsOn')
                : t('settings.notificationsOff')
            }}
          </span>
        </div>
        <UButton
          v-if="!permissions.notificationsEnabled.value"
          :label="t('settings.enableNotifications')"
          color="warning"
          size="sm"
          @click="permissions.openNotificationSettings"
        />
      </div>
    </UCard>

    <!-- Battery Optimization (Android only) -->
    <UCard v-if="vpn.isAndroid && permissions.batteryOptDisabled.value !== null" class="mb-4">
      <template #header>
        <div class="flex items-center gap-2">
          <UIcon name="i-lucide-battery" class="size-5" />
          <span class="font-semibold">{{ t('settings.batteryOptimization') }}</span>
        </div>
      </template>

      <p class="text-sm text-[var(--ui-text-muted)] mb-4">
        {{ t('settings.batteryOptimizationDescription') }}
      </p>

      <div class="flex items-center justify-between">
        <div class="flex items-center gap-2">
          <UIcon
            :name="
              permissions.batteryOptDisabled.value
                ? 'i-lucide-check-circle'
                : 'i-lucide-alert-triangle'
            "
            :class="permissions.batteryOptDisabled.value ? 'text-green-500' : 'text-yellow-500'"
            class="size-5"
          />
          <span class="text-sm">
            {{
              permissions.batteryOptDisabled.value
                ? t('settings.batteryDisabled')
                : t('settings.batteryEnabled')
            }}
          </span>
        </div>
        <UButton
          v-if="!permissions.batteryOptDisabled.value"
          :label="t('settings.disableBatteryOptimization')"
          color="warning"
          size="sm"
          @click="permissions.requestBatteryOptimization"
        />
      </div>
    </UCard>

    <!-- Split Tunneling (Android only) -->
    <UCard v-if="vpn.isAndroid">
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

        <div class="flex items-center gap-3 mb-4">
          <UInput
            v-model="searchQuery"
            :placeholder="t('settings.searchApps')"
            icon="i-lucide-search"
            class="flex-1"
          />
          <UButton
            :label="t('settings.showSystemApps')"
            :color="showSystemApps ? 'primary' : 'neutral'"
            variant="soft"
            size="sm"
            @click="showSystemApps = !showSystemApps"
          />
          <UButton
            icon="i-lucide-refresh-cw"
            color="neutral"
            variant="ghost"
            size="sm"
            :loading="settings.appsLoading"
            @click="settings.reloadApps()"
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

    <!-- Non-Android notice -->
    <UCard v-else>
      <div class="flex flex-col items-center gap-2 py-4 text-center text-[var(--ui-text-muted)]">
        <UIcon name="i-lucide-split" class="text-3xl" />
        <p>{{ t('settings.androidOnly') }}</p>
      </div>
    </UCard>

    <!-- About -->
    <UCard class="mt-4">
      <template #header>
        <div class="flex items-center gap-2">
          <UIcon name="i-lucide-info" class="size-5" />
          <span class="font-semibold">{{ t('settings.about') }}</span>
        </div>
      </template>

      <div class="flex items-center justify-between">
        <div>
          <p class="text-sm font-medium">Floppa VPN</p>
          <p class="text-xs text-[var(--ui-text-muted)]">
            {{ t('settings.version', { version: appVersion }) }}
          </p>
        </div>
        <UButton
          :label="t('changelog.whatsNew')"
          icon="i-lucide-sparkles"
          variant="soft"
          size="sm"
          @click="updateStore.openChangelogForCurrent()"
        />
      </div>
    </UCard>
  </div>
</template>
