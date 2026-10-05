import { readonly, ref } from 'vue'
import { commands, type TunnelOwner } from '../bindings'

/**
 * Who holds the tunnel this app is showing: the system service, or this process.
 *
 * Ownership is fixed for the run. Service health is refreshed on demand, so restarting
 * an incompatible or unavailable service updates the settings without changing owners.
 *
 * It matters on this side because the promises differ. "Quitting will disconnect you" is true with
 * the tunnel in this process and false with the service holding it, and a dialog that says it
 * either way is wrong half the time.
 */
const owner = ref<TunnelOwner | null>(null)
let asked: Promise<void> | null = null
let refreshing = false

export function useTunnelOwner() {
  function refresh() {
    if (refreshing && asked) return asked
    refreshing = true
    asked = commands
      .getTunnelOwner()
      .then((answer) => {
        owner.value = answer
      })
      .catch((error: unknown) => {
        console.warn(`[owner] could not ask who holds the tunnel: ${String(error)}`)
      })
      .finally(() => {
        refreshing = false
      })
    return asked
  }
  if (!asked) void refresh()

  return {
    owner: readonly(owner),
    refresh,
    /**
     * Whether quitting this app takes the tunnel with it.
     *
     * `true` while the answer is unknown: warning about a disconnect that will not happen is a
     * smaller mistake than staying silent about one that will.
     */
    quittingDisconnects: () => owner.value === null || owner.value.kind === 'in_process',
    /** A service is installed and running, and this user is not allowed to use it. */
    lockedOut: () =>
      owner.value?.kind === 'service_unavailable' && owner.value.problem.kind === 'not_permitted',
  }
}
