import { ref } from 'vue'
import { commands } from '../bindings'

export function useAndroidPermissions() {
  const batteryOptDisabled = ref<boolean | null>(null)
  const notificationsEnabled = ref<boolean | null>(null)

  const showBatteryPrompt = ref(false)
  const batteryPromptDismissed = ref(localStorage.getItem('battery_prompt_dismissed') === 'true')
  const showNotificationPrompt = ref(false)
  const notificationPromptDismissed = ref(
    localStorage.getItem('notification_prompt_dismissed') === 'true',
  )

  async function checkBatteryOptimization() {
    try {
      const result = await commands.isBatteryOptimizationDisabled()
      if (result.status === 'ok') batteryOptDisabled.value = result.data
    } catch {
      /* ignore */
    }
  }

  async function requestBatteryOptimization() {
    try {
      const result = await commands.requestDisableBatteryOptimization()
      if (result.status === 'ok') batteryOptDisabled.value = result.data
    } catch {
      /* ignore */
    }
  }

  async function checkNotifications() {
    try {
      const result = await commands.areNotificationsEnabled()
      if (result.status === 'ok') notificationsEnabled.value = result.data
    } catch {
      /* ignore */
    }
  }

  async function openNotificationSettings() {
    try {
      const result = await commands.openNotificationSettings()
      if (result.status === 'ok') notificationsEnabled.value = result.data
    } catch {
      /* ignore */
    }
  }

  async function checkPromptsAfterConnection() {
    if (!batteryPromptDismissed.value) {
      await checkBatteryOptimization()
      if (batteryOptDisabled.value === false) {
        showBatteryPrompt.value = true
      }
    }
    if (!notificationPromptDismissed.value) {
      await checkNotifications()
      if (notificationsEnabled.value === false) {
        showNotificationPrompt.value = true
      }
    }
  }

  async function handleBatteryPrompt() {
    await requestBatteryOptimization()
    dismissBatteryPrompt()
  }

  function dismissBatteryPrompt() {
    showBatteryPrompt.value = false
    batteryPromptDismissed.value = true
    localStorage.setItem('battery_prompt_dismissed', 'true')
  }

  async function handleNotificationPrompt() {
    await openNotificationSettings()
    dismissNotificationPrompt()
  }

  function dismissNotificationPrompt() {
    showNotificationPrompt.value = false
    notificationPromptDismissed.value = true
    localStorage.setItem('notification_prompt_dismissed', 'true')
  }

  return {
    batteryOptDisabled,
    notificationsEnabled,
    showBatteryPrompt,
    showNotificationPrompt,

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
}
