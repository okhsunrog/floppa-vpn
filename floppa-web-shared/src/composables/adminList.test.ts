import { describe, expect, test } from 'vite-plus/test'
import { nextTick, ref } from 'vue'

import { useAdminList, useClientPagination, useConfirmAction, useSearchFilter } from './adminList'

describe('useClientPagination', () => {
  test('slices the current page and resets to page 1 when the reset source changes', async () => {
    const items = ref(Array.from({ length: 7 }, (_, i) => i + 1))
    const search = ref('')
    const { page, paginated, pageSize } = useClientPagination(items, search, 3)

    expect(pageSize).toBe(3)
    expect(paginated.value).toEqual([1, 2, 3])

    page.value = 3
    expect(paginated.value).toEqual([7])

    search.value = 'x'
    await nextTick()
    expect(page.value).toBe(1)
    expect(paginated.value).toEqual([1, 2, 3])
  })
})

describe('useSearchFilter', () => {
  interface Row {
    name: string | null
    id: number
  }
  const rows = ref<Row[] | undefined>([
    { name: 'Alice', id: 10 },
    { name: null, id: 42 },
    { name: 'bob', id: 7 },
  ])

  test('returns everything for an empty query and matches case-insensitively', () => {
    const { search, filtered } = useSearchFilter(rows, (r) => [r.name, r.id])
    expect(filtered.value).toHaveLength(3)

    search.value = '  ALI '
    expect(filtered.value.map((r) => r.id)).toEqual([10])

    search.value = '4'
    expect(filtered.value.map((r) => r.id)).toEqual([42])
  })

  test('is empty while the list has not loaded', () => {
    const { filtered } = useSearchFilter(ref<Row[] | undefined>(undefined), (r) => [r.name])
    expect(filtered.value).toEqual([])
  })
})

describe('useConfirmAction', () => {
  test('runs the action for the requested id, including id 0, then closes', async () => {
    const { open, message, pendingId, request, confirm, reset } = useConfirmAction()
    const seen: number[] = []

    await confirm(async (id) => void seen.push(id))
    expect(seen).toEqual([]) // nothing pending: no-op

    request(0, 'delete row 0?')
    expect(open.value).toBe(true)
    expect(message.value).toBe('delete row 0?')
    expect(pendingId.value).toBe(0)

    await confirm(async (id) => void seen.push(id))
    expect(seen).toEqual([0])
    expect(open.value).toBe(false)
    expect(pendingId.value).toBeNull()

    request(5, 'x')
    reset()
    expect(open.value).toBe(false)
    expect(pendingId.value).toBeNull()
  })
})

describe('useAdminList', () => {
  test('pages the filtered list and returns to page 1 on a new search', async () => {
    const rows = ref<{ name: string }[] | undefined>(
      Array.from({ length: 5 }, (_, i) => ({ name: `row${i}` })),
    )
    const { search, filtered, page, paginated, pageSize } = useAdminList(rows, (r) => [r.name], 2)

    expect(pageSize).toBe(2)
    expect(filtered.value).toHaveLength(5)
    page.value = 3
    expect(paginated.value.map((r) => r.name)).toEqual(['row4'])

    search.value = 'row1'
    await nextTick()
    expect(page.value).toBe(1)
    expect(paginated.value.map((r) => r.name)).toEqual(['row1'])
  })
})
