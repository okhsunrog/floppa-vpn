import { defineStore } from 'pinia'
import { ref } from 'vue'
import { commands, type AppInfo, type Protocol, type SplitMode } from '../bindings'

/**
 * Probe priority used until the user reorders it, lowest first. AmneziaWG leads because plain
 * WireGuard is DPI-blocked on the networks this client targets.
 *
 * A `Record` and not an array: `Protocol` is generated from Rust, so this stops compiling the
 * moment a protocol is added there and nobody has said where it belongs. As a list it would
 * simply have been short, and the missing protocol would have been unreachable in auto-select.
 */
const DEFAULT_PRIORITY: Record<Protocol, number> = {
  amneziawg: 0,
  wireguard: 1,
  vless: 2,
}

const DEFAULT_PROTOCOL_ORDER: Protocol[] = (Object.keys(DEFAULT_PRIORITY) as Protocol[]).sort(
  (a, b) => DEFAULT_PRIORITY[a] - DEFAULT_PRIORITY[b],
)

const KNOWN_PROTOCOLS = new Set<string>(DEFAULT_PROTOCOL_ORDER)

function isProtocol(value: unknown): value is Protocol {
  return typeof value === 'string' && KNOWN_PROTOCOLS.has(value)
}

/** Persisted orders are user data from an older build: drop anything that is no longer a protocol,
 *  then append protocols added since, so the list is always exactly the known set. */
function sanitizeProtocolOrder(stored: unknown): Protocol[] {
  const kept = Array.isArray(stored) ? stored.filter(isProtocol) : []
  const deduped = [...new Set(kept)]
  return [...deduped, ...DEFAULT_PROTOCOL_ORDER.filter((p) => !deduped.includes(p))]
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
      ],
      // localStorage holds whatever an older build wrote. Narrow it back to Protocol[] on load,
      // so an unknown string can never reach `t(\`vpn.${proto}\`)` or a probe order.
      afterHydrate: (ctx) => {
        ctx.store.protocolOrder = sanitizeProtocolOrder(ctx.store.protocolOrder)
        if (!isProtocol(ctx.store.manualProtocol)) ctx.store.manualProtocol = null

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
