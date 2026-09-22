import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { getMyRegions, setMyRegion } from 'floppa-web-shared/client/sdk.gen'
import type { RegionInfo } from 'floppa-web-shared/client/types.gen'

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'object' && error !== null && 'message' in error) {
    const message = (error as { message?: unknown }).message
    if (typeof message === 'string') return message
  }
  return String(error)
}

export const useRegionStore = defineStore('regions', () => {
  const regions = ref<RegionInfo[]>([])
  const loading = ref(false)
  const changing = ref(false)
  const error = ref<string | null>(null)

  const availableRegions = computed(() => regions.value.filter((region) => region.available))
  const selected = computed(() => regions.value.find((region) => region.selected) ?? null)
  const supportsVless = computed(() => selected.value?.supports_vless ?? true)

  async function refresh(): Promise<void> {
    loading.value = true
    error.value = null
    try {
      const response = await getMyRegions({ throwOnError: true })
      regions.value = response.data
    } catch (e) {
      error.value = errorMessage(e)
    } finally {
      loading.value = false
    }
  }

  async function select(regionId: string): Promise<boolean> {
    const region = regions.value.find((item) => item.id === regionId)
    if (!region?.available || region.selected) return false

    changing.value = true
    error.value = null
    try {
      await setMyRegion({ body: { region_id: regionId }, throwOnError: true })
      regions.value = regions.value.map((item) => ({
        ...item,
        selected: item.id === regionId,
      }))
      return true
    } catch (e) {
      error.value = errorMessage(e)
      return false
    } finally {
      changing.value = false
    }
  }

  return {
    regions,
    availableRegions,
    selected,
    supportsVless,
    loading,
    changing,
    error,
    refresh,
    select,
  }
})
