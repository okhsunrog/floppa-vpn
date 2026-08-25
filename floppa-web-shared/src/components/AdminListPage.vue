<script setup lang="ts" generic="T">
import { computed, useSlots } from 'vue'
import type { TableColumn, TableRow } from '@nuxt/ui'

/**
 * The skeleton every admin list shares: a title with a search box, the query's loading and
 * error states, a table on wide screens and one card per row on narrow ones, and pagination.
 *
 * Generic over the row type, so a `#<column>-cell` slot receives a `TableRow<T>` and the `card`
 * slot a `T` — the views used to type those as `any`. What a row *is* stays with the view: the
 * columns, the searchable fields, the cell rendering, the actions and any dialog it opens (those
 * go in the default slot). Filtering and paging come from `useAdminList`.
 */
const props = withDefaults(
  defineProps<{
    title: string
    /** As Pinia Colada reports it. */
    status: 'pending' | 'error' | 'success'
    error?: { message: string } | null
    columns: TableColumn<T>[]
    /** The current page. */
    rows: T[]
    /** How many rows match the search — drives the empty state and the pagination. */
    total: number
    pageSize: number
    rowKey: (row: T) => string | number
    searchPlaceholder: string
    emptyText: string
    /** Tailwind max-width of the page. */
    containerClass?: string
  }>(),
  { error: null, containerClass: 'max-w-7xl' },
)

const page = defineModel<number>('page', { required: true })
const search = defineModel<string>('search', { required: true })

const emit = defineEmits<{
  /** A row was clicked, in either layout. */
  select: [row: T]
}>()

defineSlots<{
  /** Rendered next to the search box. */
  'header-actions'?: () => unknown
  /** One row on a narrow screen. */
  card: (props: { row: T }) => unknown
  /** Dialogs and anything else that belongs to the page but not to the list. */
  default?: () => unknown
  /** Passed through to `UTable`. */
  [cell: `${string}-cell`]: (props: { row: TableRow<T> }) => unknown
}>()

type CellSlot = `${string}-cell`

const slots = useSlots()
const cellSlots = computed(() =>
  Object.keys(slots).filter((name): name is CellSlot => name.endsWith('-cell')),
)

const showPagination = computed(() => props.total > props.pageSize)
</script>

<template>
  <div :class="[containerClass, 'mx-auto']">
    <div class="flex justify-between items-center mb-6 flex-wrap gap-4">
      <h1 class="text-2xl font-bold">{{ title }}</h1>
      <div class="flex items-center gap-2 w-full sm:w-auto">
        <UInput
          v-model="search"
          :placeholder="searchPlaceholder"
          icon="i-lucide-search"
          class="flex-1 sm:w-64"
        />
        <slot name="header-actions" />
      </div>
    </div>

    <div v-if="status === 'pending'" class="flex justify-center py-12">
      <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
    </div>
    <UAlert v-else-if="error" color="error" :title="error.message" />
    <template v-else>
      <!-- Desktop table -->
      <div class="hidden md:block">
        <UTable
          :data="rows"
          :columns="columns"
          class="[&_tbody_tr]:cursor-pointer"
          @select="(_e: Event, row: TableRow<T>) => emit('select', row.original)"
        >
          <template v-for="name in cellSlots" :key="name" #[name]="cellProps">
            <slot :name="name" v-bind="cellProps" />
          </template>
          <template #empty>
            <div class="text-center py-8 text-[var(--ui-text-muted)]">
              {{ emptyText }}
            </div>
          </template>
        </UTable>
      </div>

      <!-- Mobile cards -->
      <div class="md:hidden flex flex-col gap-3">
        <div v-if="total === 0" class="text-center py-8 text-[var(--ui-text-muted)]">
          {{ emptyText }}
        </div>
        <UCard
          v-for="row in rows"
          :key="rowKey(row)"
          class="cursor-pointer active:scale-[0.98] transition-transform"
          @click="() => emit('select', row)"
        >
          <slot name="card" :row="row" />
        </UCard>
      </div>

      <div v-if="showPagination" class="flex justify-center mt-4">
        <UPagination v-model:page="page" :total="total" :items-per-page="pageSize" />
      </div>
    </template>

    <slot />
  </div>
</template>
