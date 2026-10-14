import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import { commands, type AppInfo, type Protocol, type SplitMode, type Settings } from '../bindings'
import { DEFAULT_PROTOCOL_ORDER } from '../utils/protocolOrder'

/**
 * What pressing the window's close button does on desktop.
 *
 * `'ask'` until the user has answered once, whatever the tunnel is doing at the time — a rule
 * that skipped the question while disconnected hid the tray from anyone who closed the app before
 * first connecting. Persisted here rather than in Rust because it is a setting, and this is where
 * the settings are; Rust prevents every close and lets this decide (see `src-tauri/src/tray.rs`).
 */
export type CloseBehavior = 'ask' | 'tray' | 'quit'

const CLOSE_BEHAVIORS: CloseBehavior[] = ['ask', 'tray', 'quit']

function isCloseBehavior(value: unknown): value is CloseBehavior {
  return CLOSE_BEHAVIORS.includes(value as CloseBehavior)
}

export const useSettingsStore = defineStore(
  'vpn-settings',
  () => {
    const splitMode = ref<SplitMode>('all')
    const selectedApps = ref<string[]>([])
    const excludedRoutes = ref<string[]>([])
    const allowLan = ref(false)
    const showConnectionDetails = ref(false)

    // When true (default), connecting auto-probes protocols in order and stays on
    // the first that works. When false, the user picks the protocol manually via
    // the switcher on the connection card.
    const autoSelect = ref(true)

    // User-defined probe order for auto-select (most preferred first). Editable in
    // the Protocol settings modal.
    const protocolOrder = ref<Protocol[]>([...DEFAULT_PROTOCOL_ORDER])

    // The protocol picked on the connection card's switcher when auto-select is off. Kept apart
    // from `protocolOrder`: a manual pick is a choice for the next connect, not a reordering of
    // the auto-select priority. `null` until the user has picked one.
    const manualProtocol = ref<Protocol | null>(null)

    // Desktop only: Android's back gesture does not close anything, and the tunnel there lives
    // in a process of its own.
    const closeBehavior = ref<CloseBehavior>('ask')

    const settingsError = ref<string | null>(null)
    const settingsReady = ref(false)
    let revision = 0
    let saved: Settings | null = null
    let applying = false
    let loading: Promise<void> | null = null
    let pending: Promise<void> = Promise.resolve()

    function draft(): Settings {
      if (!saved) throw new Error('Settings have not loaded')
      return {
        ...saved,
        protocols: {
          mode: autoSelect.value ? 'auto' : 'manual',
          auto_order: [...protocolOrder.value],
          manual: manualProtocol.value ?? saved.protocols.manual,
        },
        routing: {
          allow_lan: allowLan.value,
          excluded_routes: [...excludedRoutes.value],
          apps: { mode: splitMode.value, items: [...selectedApps.value] },
        },
      }
    }

    async function loadOwnerSettings(): Promise<void> {
      loading ??= (async () => {
        const response = await commands.tunnelGetSettings()
        if (response.status === 'error') throw new Error(JSON.stringify(response.error))
        const doc = response.data.document
        applying = true
        try {
          saved = {
            protocols: doc.protocols,
            routing: doc.routing,
            recovery: doc.recovery,
            startup: doc.startup,
          }
          revision = doc.revision
          autoSelect.value = saved.protocols.mode === 'auto'
          protocolOrder.value = [...saved.protocols.auto_order]
          manualProtocol.value = saved.protocols.manual
          allowLan.value = saved.routing.allow_lan
          excludedRoutes.value = [...saved.routing.excluded_routes]
          splitMode.value = saved.routing.apps.mode
          selectedApps.value = [...saved.routing.apps.items]
          settingsError.value = null
          settingsReady.value = true
        } finally {
          applying = false
        }
      })().catch((error: unknown) => {
        loading = null
        settingsError.value = String(error)
        throw error
      })
      return loading
    }

    // Serialize writes through the owner. A conflict stops the queue instead of silently
    // overwriting another editor's changes. Nothing here persists VPN preferences locally.
    watch(
      [
        autoSelect,
        protocolOrder,
        manualProtocol,
        allowLan,
        excludedRoutes,
        splitMode,
        selectedApps,
      ],
      () => {
        if (applying || !settingsReady.value || settingsError.value) return
        const next = draft()
        pending = pending
          .then(async () => {
            if (settingsError.value) return
            const response = await commands.tunnelUpdateSettings(revision, next)
            if (response.status === 'error') throw new Error(JSON.stringify(response.error))
            revision = response.data.document.revision
            const doc = response.data.document
            saved = {
              protocols: doc.protocols,
              routing: doc.routing,
              recovery: doc.recovery,
              startup: doc.startup,
            }
          })
          .catch((error: unknown) => {
            settingsError.value = String(error)
          })
      },
      { deep: true, flush: 'sync' },
    )

    async function flushSettings(): Promise<void> {
      await loadOwnerSettings()
      await pending
      if (settingsError.value) throw new Error(settingsError.value)
    }

    async function reloadOwnerSettings(): Promise<void> {
      await pending
      loading = null
      settingsReady.value = false
      await loadOwnerSettings()
    }

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

    async function reloadApps(): Promise<AppInfo[]> {
      cachedApps.value = null
      return loadApps()
    }

    return {
      splitMode,
      selectedApps,
      allowLan,
      excludedRoutes,
      showConnectionDetails,
      autoSelect,
      protocolOrder,
      manualProtocol,
      settingsError,
      settingsReady,
      loadOwnerSettings,
      flushSettings,
      reloadOwnerSettings,
      closeBehavior,
      cachedApps,
      appsLoading,
      toggleApp,
      clearSelectedApps,
      loadApps,
      reloadApps,
    }
  },
  {
    persist: {
      pick: ['showConnectionDetails', 'closeBehavior'],
      afterHydrate: (ctx) => {
        if (!isCloseBehavior(ctx.store.closeBehavior)) ctx.store.closeBehavior = 'ask'
      },
    },
  },
)
