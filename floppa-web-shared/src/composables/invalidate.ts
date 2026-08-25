import { useQueryCache, type EntryKey } from '@pinia/colada'

/**
 * Returns `invalidate(...keys)`: marks every cached query matching one of the keys as stale and
 * refetches the active ones, so every consumer of that data updates — not just the view that
 * ran the mutation (e.g. `getMyPeers` is read by both the dashboard and the peers page).
 *
 * This is what a mutation should call, not `refresh()` from `useQuery`: `refresh` only fetches
 * when the entry is already stale (default `staleTime` is 5 s), so a create/delete right after
 * page load — or two actions in a row — would keep showing the old list.
 *
 * Keys match by prefix, so a generated `xxxQueryKey()` called without path params invalidates
 * every entry of that endpoint (all `getUser` ids, for example).
 */
export function useInvalidateQueries() {
  const queryCache = useQueryCache()
  return async (...keys: EntryKey[]): Promise<void> => {
    await Promise.all(keys.map((key) => queryCache.invalidateQueries({ key })))
  }
}
