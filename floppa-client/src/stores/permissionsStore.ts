import { defineStore } from 'pinia'
import { ref } from 'vue'
import { commands } from '../bindings'

/**
 * The two Android permissions we ask about, and whether the user has waved the prompt away.
 *
 * A store rather than a composable, for two reasons. It holds state that must persist, and it was
 * reaching for `localStorage` directly while the rest of the app persisted through
 * `pinia-plugin-persistedstate` — two mechanisms for one job. And it was instantiated separately
 * by the connection card and the settings screen, so dismissing a prompt in one left the other
 * holding its own copy of the flag; a store is shared by construction.
 */
export const usePermissionsStore = defineStore(
  'android-permissions',
  () => {
    /** `null` until asked. Distinct from `false`, which is an answer. */
    const batteryOptDisabled = ref<boolean | null>(null)
    const notificationsEnabled = ref<boolean | null>(null)

    const showBatteryPrompt = ref(false)
    const showNotificationPrompt = ref(false)

    const batteryPromptDismissed = ref(false)
    const notificationPromptDismissed = ref(false)

    async function checkBatteryOptimization() {
      try {
        const result = await commands.isBatteryOptimizationDisabled()
        if (result.status === 'ok') batteryOptDisabled.value = result.data
      } catch (e) {
        console.warn('[permissions] failed to read battery optimization state:', e)
      }
    }

    async function requestBatteryOptimization() {
      try {
        const result = await commands.requestDisableBatteryOptimization()
        if (result.status === 'ok') batteryOptDisabled.value = result.data
      } catch (e) {
        console.warn('[permissions] failed to request battery optimization:', e)
      }
    }

    async function checkNotifications() {
      try {
        const result = await commands.areNotificationsEnabled()
        if (result.status === 'ok') notificationsEnabled.value = result.data
      } catch (e) {
        console.warn('[permissions] failed to read notification state:', e)
      }
    }

    async function openNotificationSettings() {
      try {
        const result = await commands.openNotificationSettings()
        if (result.status === 'ok') notificationsEnabled.value = result.data
      } catch (e) {
        console.warn('[permissions] failed to open notification settings:', e)
      }
    }

    /** Asked once a tunnel has actually come up, so the prompts arrive with a reason. */
    async function checkPromptsAfterConnection() {
      if (!batteryPromptDismissed.value) {
        await checkBatteryOptimization()
        if (batteryOptDisabled.value === false) showBatteryPrompt.value = true
      }
      if (!notificationPromptDismissed.value) {
        await checkNotifications()
        if (notificationsEnabled.value === false) showNotificationPrompt.value = true
      }
    }

    function dismissBatteryPrompt() {
      showBatteryPrompt.value = false
      batteryPromptDismissed.value = true
    }

    async function handleBatteryPrompt() {
      await requestBatteryOptimization()
      dismissBatteryPrompt()
    }

    function dismissNotificationPrompt() {
      showNotificationPrompt.value = false
      notificationPromptDismissed.value = true
    }

    async function handleNotificationPrompt() {
      await openNotificationSettings()
      dismissNotificationPrompt()
    }

    return {
      batteryOptDisabled,
      notificationsEnabled,
      showBatteryPrompt,
      showNotificationPrompt,
      batteryPromptDismissed,
      notificationPromptDismissed,
      checkBatteryOptimization,
      checkNotifications,
      requestBatteryOptimization,
      openNotificationSettings,
      checkPromptsAfterConnection,
      handleBatteryPrompt,
      dismissBatteryPrompt,
      handleNotificationPrompt,
      dismissNotificationPrompt,
    }
  },
  {
    // Only the dismissals survive a restart. Whether a permission is currently granted is asked
    // of Android every time — the user can change it in system settings while we are not looking.
    persist: { pick: ['batteryPromptDismissed', 'notificationPromptDismissed'] },
  },
)
