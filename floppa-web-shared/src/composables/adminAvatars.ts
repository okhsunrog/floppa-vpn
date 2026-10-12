import { ref, watch, type Ref } from 'vue'
import { getAvatarsBatch } from '../client/sdk.gen'

/** Load only visible users through the authenticated server cache, not Telegram's CDN. */
export function useAdminAvatars(ids: () => number[]): Ref<Record<string, string>> {
  const avatars = ref<Record<string, string>>({})
  const requested = new Set<number>()
  watch(
    ids,
    async (visible) => {
      const missing = [...new Set(visible)].filter((id) => !requested.has(id))
      missing.forEach((id) => requested.add(id))
      for (let start = 0; start < missing.length; start += 200) {
        const batch = missing.slice(start, start + 200)
        try {
          const { data, error } = await getAvatarsBatch({ body: { user_ids: batch } })
          if (error) batch.forEach((id) => requested.delete(id))
          if (data) avatars.value = { ...avatars.value, ...data }
        } catch {
          batch.forEach((id) => requested.delete(id))
        }
      }
    },
    { immediate: true },
  )
  return avatars
}
