import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { getMyPrivateNetworks } from 'floppa-web-shared/client/sdk.gen'
import type { PrivateNetworkInfo } from 'floppa-web-shared/client/types.gen'

/**
 * The private networks the account's plan grants (`GET /me/private-networks`). Empty for almost
 * everyone. Their CIDRs go into `TunnelParams.private_routes`, so a WireGuard-family tunnel
 * routes them on top of everything else, the LAN bypass included.
 *
 * Only routes are decided here: the server allows or refuses the traffic on its side whatever
 * this list says. The list is persisted so a connect with the server out of reach (a boot, a
 * reconnect) still carries the last known routes.
 */
export const usePrivateNetworksStore = defineStore(
  'privateNetworks',
  () => {
    const networks = ref<PrivateNetworkInfo[]>([])

    /** Sorted and deduplicated, the way `TunnelParams::with_private_routes` writes them. */
    const routes = computed(() => [...new Set(networks.value.flatMap((n) => n.cidrs))].sort())

    async function refresh(): Promise<void> {
      try {
        const response = await getMyPrivateNetworks({ throwOnError: true })
        networks.value = response.data
      } catch {
        // Keep the last known list: failing to ask is not the plan losing its networks.
      }
    }

    return { networks, routes, refresh }
  },
  { persist: { pick: ['networks'] } },
)
