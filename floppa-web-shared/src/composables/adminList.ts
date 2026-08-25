import { ref, computed, watch, type ComputedRef, type Ref } from 'vue'

/** Page size for admin list views — the list queries return all rows, so paging is client-side. */
export const ADMIN_PAGE_SIZE = 100

/**
 * Case-insensitive substring search over a list. `fields` picks the searchable text of one row
 * (nullable entries are skipped); an empty query returns the whole list. Returns the `search`
 * model for the input and the `filtered` list, which is what `useClientPagination` consumes.
 */
export function useSearchFilter<T>(
  items: Ref<readonly T[] | undefined>,
  fields: (item: T) => readonly (string | number | null | undefined)[],
) {
  const search = ref('')
  const filtered = computed<T[]>(() => {
    const list = items.value ?? []
    const q = search.value.trim().toLowerCase()
    if (!q) return [...list]
    return list.filter((item) =>
      fields(item).some((f) => f != null && String(f).toLowerCase().includes(q)),
    )
  })
  return { search, filtered }
}

/**
 * Client-side pagination over an already-filtered list. Resets to page 1 whenever `resetOn`
 * changes (typically the search query). Returns the current `page`, the `paginated` slice, and
 * the `pageSize` in use.
 */
export function useClientPagination<T>(
  filtered: Ref<T[]> | ComputedRef<T[]>,
  resetOn: Ref<unknown>,
  pageSize = ADMIN_PAGE_SIZE,
) {
  const page = ref(1)
  const paginated = computed(() =>
    filtered.value.slice((page.value - 1) * pageSize, page.value * pageSize),
  )
  watch(resetOn, () => {
    page.value = 1
  })
  return { page, paginated, pageSize }
}

/**
 * Confirmation-modal state for a single-row action (delete, regenerate, …).
 * `request(id, message)` opens the modal for a row; `confirm(action)` runs `action(id)` for the
 * pending row then closes. `message` is a free-form string the view sets (a prebuilt confirm
 * message, a username, …). The `id === null` guard correctly handles a row id of 0.
 */
export function useConfirmAction() {
  const open = ref(false)
  const message = ref('')
  const pendingId = ref<number | null>(null)

  function request(id: number, msg: string) {
    pendingId.value = id
    message.value = msg
    open.value = true
  }

  function reset() {
    open.value = false
    pendingId.value = null
  }

  async function confirm(action: (id: number) => Promise<void>) {
    if (pendingId.value === null) return
    await action(pendingId.value)
    reset()
  }

  return { open, message, pendingId, request, confirm, reset }
}
