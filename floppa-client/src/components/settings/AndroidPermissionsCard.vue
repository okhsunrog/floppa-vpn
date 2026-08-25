<script setup lang="ts">
import { onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { usePermissionsStore } from '../../stores/permissionsStore'

/**
 * The two Android permissions the app depends on. Each card shows only once Android has been
 * asked, so nothing flashes a wrong answer on the way in.
 */
const { t } = useI18n()
const permissions = usePermissionsStore()

onMounted(() => {
  permissions.checkBatteryOptimization()
  permissions.checkNotifications()
})
</script>

<template>
  <!-- Notifications -->
  <UCard v-if="permissions.notificationsEnabled !== null" class="mb-4">
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
            permissions.notificationsEnabled ? 'i-lucide-check-circle' : 'i-lucide-alert-triangle'
          "
          :class="permissions.notificationsEnabled ? 'text-green-500' : 'text-yellow-500'"
          class="size-5"
        />
        <span class="text-sm">
          {{
            permissions.notificationsEnabled
              ? t('settings.notificationsOn')
              : t('settings.notificationsOff')
          }}
        </span>
      </div>
      <UButton
        v-if="!permissions.notificationsEnabled"
        :label="t('settings.enableNotifications')"
        color="warning"
        size="sm"
        @click="permissions.openNotificationSettings"
      />
    </div>
  </UCard>

  <!-- Battery Optimization -->
  <UCard v-if="permissions.batteryOptDisabled !== null" class="mb-4">
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
            permissions.batteryOptDisabled ? 'i-lucide-check-circle' : 'i-lucide-alert-triangle'
          "
          :class="permissions.batteryOptDisabled ? 'text-green-500' : 'text-yellow-500'"
          class="size-5"
        />
        <span class="text-sm">
          {{
            permissions.batteryOptDisabled
              ? t('settings.batteryDisabled')
              : t('settings.batteryEnabled')
          }}
        </span>
      </div>
      <UButton
        v-if="!permissions.batteryOptDisabled"
        :label="t('settings.disableBatteryOptimization')"
        color="warning"
        size="sm"
        @click="permissions.requestBatteryOptimization"
      />
    </div>
  </UCard>
</template>
