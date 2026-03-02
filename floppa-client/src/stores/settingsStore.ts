import { defineStore } from 'pinia'
import { ref } from 'vue'
import { commands, type AppInfo } from '../bindings'

export type SplitMode = 'all' | 'include' | 'exclude'

export const useSettingsStore = defineStore(
  'vpn-settings',
  () => {
    const splitMode = ref<SplitMode>('all')
    const selectedApps = ref<string[]>([])

    // Cached app list (not persisted — fetched once per session)
    const cachedApps = ref<AppInfo[] | null>(null)
    const appsLoading = ref(false)

    function toggleApp(packageName: string) {
      const idx = selectedApps.value.indexOf(packageName)
      if (idx >= 0) {
        selectedApps.value.splice(idx, 1)
      } else {
        selectedApps.value.push(packageName)
      }
    }

    function clearSelectedApps() {
      selectedApps.value = []
    }

    async function loadApps(): Promise<AppInfo[]> {
      if (cachedApps.value) return cachedApps.value

      appsLoading.value = true
      try {
        const result = await commands.getInstalledApps()
        if (result.status === 'ok') {
          cachedApps.value = result.data
          return result.data
        }
        console.error('Failed to load installed apps:', result.error)
        return []
      } catch (e) {
        console.error('Failed to load installed apps:', e)
        return []
      } finally {
        appsLoading.value = false
      }
    }

    return {
      splitMode,
      selectedApps,
      cachedApps,
      appsLoading,
      toggleApp,
      clearSelectedApps,
      loadApps,
    }
  },
  {
    persist: {
      pick: ['splitMode', 'selectedApps'],
    },
  },
)
