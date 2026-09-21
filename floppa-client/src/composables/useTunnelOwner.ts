import { readonly, ref } from 'vue'
import { commands, type TunnelOwner } from '../bindings'

/**
 * Who holds the tunnel this app is showing: the system service, or this process.
 *
 * Asked once and cached for the run, because that is exactly how long the answer is good for —
 * Rust decides it during setup and nothing changes it afterwards. Deliberately so: two actors must
 * never both be live, so the mode a client picked is the mode it keeps until it is restarted.
 *
 * It matters on this side because the promises differ. "Quitting will disconnect you" is true with
 * the tunnel in this process and false with the service holding it, and a dialog that says it
 * either way is wrong half the time.
 */
const owner = ref<TunnelOwner | null>(null)
let asked: Promise<void> | null = null

export function useTunnelOwner() {
  asked ??= commands
    .getTunnelOwner()
    .then((answer) => {
      owner.value = answer
    })
    .catch((error: unknown) => {
      // Not fatal, and not worth a dialog: every caller treats "not known yet" the same way it
      // treats the in-process case, which is the more cautious of the two to be wrong about.
      console.warn(`[owner] could not ask who holds the tunnel: ${String(error)}`)
    })

  return {
    owner: readonly(owner),
    /**
     * Whether quitting this app takes the tunnel with it.
     *
     * `true` while the answer is unknown: warning about a disconnect that will not happen is a
     * smaller mistake than staying silent about one that will.
     */
    quittingDisconnects: () => owner.value?.kind !== 'service',
    /** A service is installed and running, and this user is not allowed to use it. */
    lockedOut: () =>
      owner.value?.kind === 'in_process' && owner.value.reason?.kind === 'not_permitted',
  }
}
