import { defineStore } from 'pinia'
import { ref } from 'vue'
import { commands, type AppInfo, type Protocol, type SplitMode } from '../bindings'
import { DEFAULT_PROTOCOL_ORDER, isProtocol, sanitizeProtocolOrder } from '../utils/protocolOrder'

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

    // One-time guard for the upgrade to auto-select; applied in `afterHydrate` below.
    const protocolDefaultsApplied = ref(false)

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
      autoSelect,
      protocolOrder,
      manualProtocol,
      protocolDefaultsApplied,
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
      pick: [
        'splitMode',
        'selectedApps',
        'autoSelect',
        'protocolOrder',
        'manualProtocol',
        'protocolDefaultsApplied',
        'closeBehavior',
      ],
      // localStorage holds whatever an older build wrote. Narrow it back to Protocol[] on load,
      // so an unknown string can never reach `t(\`vpn.${proto}\`)` or a probe order.
      afterHydrate: (ctx) => {
        ctx.store.protocolOrder = sanitizeProtocolOrder(ctx.store.protocolOrder)
        if (!isProtocol(ctx.store.manualProtocol)) ctx.store.manualProtocol = null
        if (!isCloseBehavior(ctx.store.closeBehavior)) ctx.store.closeBehavior = 'ask'

        // One-time on upgrade to auto-select: enable it and forget the previously-used protocol,
        // so the first cycle probes from the configured priority instead of inheriting an old
        // manual pick. A migration of persisted state, so it lives with the hydration — not in
        // whichever component happens to mount first.
        if (!ctx.store.protocolDefaultsApplied) {
          ctx.store.autoSelect = true
          ctx.store.protocolDefaultsApplied = true
          commands.forgetPreferredProtocol().catch((e: unknown) => {
            console.error('[settingsStore] failed to forget the preferred protocol:', e)
          })
        }
      },
    },
  },
)
