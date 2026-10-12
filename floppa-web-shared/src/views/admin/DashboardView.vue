<script setup lang="ts">
import AdminUserIdentity from '../../components/AdminUserIdentity.vue'
import { useAdminAvatars } from '../../composables/adminAvatars'
import { computed, ref, watch } from 'vue'
import { useMediaQuery } from '@vueuse/core'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getAnalyticsQuery, getAnalyticsQueryKey } from '../../client/@pinia/colada.gen'
import { useInvalidateQueries } from '../../composables/invalidate'
import {
  paymentTimeline,
  summarizeAnalytics,
  USAGE_THRESHOLD,
} from '../../composables/adminAnalytics'
import { ADMIN_NAVIGATION } from '../../composables/adminNavigation'
import { formatTraffic } from '../../utils'

const { t, locale } = useI18n()
const compactChart = useMediaQuery('(max-width: 639px)')
const route = useRoute()
const router = useRouter()
const today = new Date().toISOString().slice(0, 10)
const daysAgo = (days: number) =>
  new Date(Date.parse(`${today}T00:00:00Z`) - (days - 1) * 86400000).toISOString().slice(0, 10)
const from = ref(typeof route.query.from === 'string' ? route.query.from : daysAgo(30))
const to = ref(typeof route.query.to === 'string' ? route.query.to : today)
const valid = computed(
  () =>
    /^\d{4}-\d{2}-\d{2}$/.test(from.value) &&
    /^\d{4}-\d{2}-\d{2}$/.test(to.value) &&
    from.value <= to.value &&
    to.value <= today &&
    (Date.parse(to.value) - Date.parse(from.value)) / 86400000 < 366,
)
const applied = ref({ from: daysAgo(30), to: today })
if (valid.value) applied.value = { from: from.value, to: to.value }
const { data, status, asyncStatus, error } = useQuery(() =>
  getAnalyticsQuery({ query: applied.value }),
)
const invalidate = useInvalidateQueries()
function applyPeriod() {
  if (!valid.value) return
  applied.value = { from: from.value, to: to.value }
  windowFilter.value = null
  void router.replace({ query: { ...route.query, ...applied.value } })
}
function preset(days: number) {
  from.value = daysAgo(days)
  to.value = today
  applyPeriod()
}
const summary = computed(() => (data.value ? summarizeAnalytics(data.value) : null))
const timeline = computed(() =>
  data.value
    ? paymentTimeline(data.value, applied.value.from, applied.value.to, compactChart.value)
    : [],
)
const chartMax = computed(() => Math.max(1, ...timeline.value.map((b) => b.first + b.repeat)))
const date = (value: string) =>
  new Date(`${value.slice(0, 10)}T00:00:00Z`).toLocaleDateString(locale.value, {
    day: 'numeric',
    month: 'short',
    timeZone: 'UTC',
  })
const number = (value: number | undefined | null) =>
  value == null ? '—' : value.toLocaleString(locale.value)
const sourceLabel = (source: string | null | undefined) => t(`analytics.${source ?? 'none'}`)
const selected = ref('payments')
const search = ref('')
const plan = ref('all')
const page = ref(1)
const sort = ref<'date' | 'name' | 'amount'>('date')
const descending = ref(true)
const windowFilter = ref<{ from: string; to: string } | null>(null)
const details = ref<HTMLElement>()
function drilldown(kind: string) {
  selected.value = kind
  search.value = ''
  plan.value = 'all'
  windowFilter.value = null
  page.value = 1
  details.value?.scrollIntoView({ behavior: 'auto', block: 'start' })
}
function selectBar(bin: { from: string; to: string }) {
  drilldown('payments')
  windowFilter.value = bin
}
function selectPlan(name: string) {
  drilldown('current_purchase')
  plan.value = name
}
const kinds = [
  'payments',
  'first',
  'repeat',
  'admin_grant',
  'trials',
  'credit',
  'current_purchase',
  'current_admin_grant',
  'current_trials',
  'usage',
]
const kindItems = computed(() => kinds.map((value) => ({ value, label: t(`analytics.${value}`) })))
const planItems = computed(() => [
  { value: 'all', label: t('adminWorkspace.allPlans') },
  ...[
    ...new Set([
      ...(data.value?.events.map((e) => e.plan) ?? []),
      ...(data.value?.access.flatMap((a) => (a.plan ? [a.plan] : [])) ?? []),
    ]),
  ]
    .sort()
    .map((value) => ({ value, label: value })),
])
const metrics = computed(() => [
  {
    kind: 'first',
    value: summary.value?.first,
    label: t('analytics.first'),
    hint: t('analytics.people'),
  },
  {
    kind: 'repeat',
    value: summary.value?.repeat,
    label: t('analytics.repeat'),
    hint: t('analytics.people'),
  },
  {
    kind: 'payments',
    value: summary.value?.revenue,
    label: t('analytics.revenue'),
    hint: t('analytics.stars'),
  },
  {
    kind: 'usage',
    value: summary.value?.usage?.length,
    label: t('analytics.usage'),
    hint: t('analytics.people'),
  },
])
const accessCounts = computed(() =>
  ['purchase', 'admin_grant', 'trials'].map((source) => ({
    source,
    count: data.value?.access.filter((a) =>
      source === 'trials' ? a.source === 'trial' || a.source === 'taster' : a.source === source,
    ).length,
  })),
)
const paidPlans = computed(() => {
  const counts = new Map<string, number>()
  for (const a of data.value?.access ?? [])
    if (a.source === 'purchase' && a.plan) counts.set(a.plan, (counts.get(a.plan) ?? 0) + 1)
  return [...counts].sort((a, b) => b[1] - a[1])
})
type Row = {
  id: string
  user: number
  name: string
  plan: string
  kind: string
  date: string
  amount: number | null
  traffic: number | null
}
const rows = computed<Row[]>(() => {
  if (!data.value) return []
  if (selected.value === 'usage')
    return (summary.value?.usage ?? []).map((u) => {
      const a = data.value!.access.find((a) => a.user_id === u.user_id)
      return {
        id: `usage-${u.user_id}`,
        user: u.user_id,
        name: a?.name ?? `#${u.user_id}`,
        plan: a?.plan ?? '—',
        kind: a?.source ?? 'none',
        date: '',
        amount: null,
        traffic: u.download + u.upload,
      }
    })
  if (selected.value.startsWith('current_'))
    return data.value.access
      .filter((a) =>
        selected.value === 'current_trials'
          ? a.source === 'trial' || a.source === 'taster'
          : a.source === selected.value.replace('current_', ''),
      )
      .map((a) => ({
        id: `access-${a.user_id}`,
        user: a.user_id,
        name: a.name,
        plan: a.plan ?? '—',
        kind: a.source ?? 'none',
        date: '',
        amount: null,
        traffic: null,
      }))
  return data.value.events
    .filter((e) =>
      selected.value === 'payments'
        ? e.kind === 'first' || e.kind === 'repeat'
        : selected.value === 'trials'
          ? e.kind === 'trial' || e.kind === 'taster'
          : e.kind === selected.value,
    )
    .map((e) => ({
      id: e.id,
      user: e.user_id,
      name: e.name,
      plan: e.plan,
      kind: e.kind,
      date: e.at,
      amount: e.amount,
      traffic: null,
    }))
})
const filtered = computed(() =>
  rows.value
    .filter(
      (r) =>
        (plan.value === 'all' || r.plan === plan.value) &&
        (!windowFilter.value ||
          (r.date.slice(0, 10) >= windowFilter.value.from &&
            r.date.slice(0, 10) <= windowFilter.value.to)) &&
        `${r.name} ${r.user}`.toLocaleLowerCase().includes(search.value.trim().toLocaleLowerCase()),
    )
    .sort((a, b) => {
      const cmp =
        sort.value === 'name'
          ? a.name.localeCompare(b.name, locale.value, { numeric: true })
          : sort.value === 'amount'
            ? (a.traffic ?? a.amount ?? 0) - (b.traffic ?? b.amount ?? 0)
            : a.date.localeCompare(b.date)
      return (descending.value ? -cmp : cmp) || a.id.localeCompare(b.id)
    }),
)
const visible = computed(() => filtered.value.slice((page.value - 1) * 10, page.value * 10))
watch([selected, search, plan, sort, descending, applied, windowFilter], () => {
  page.value = 1
})
watch(
  () => filtered.value.length,
  (n) => {
    page.value = Math.min(page.value, Math.max(1, Math.ceil(n / 10)))
  },
)
function changeSort(key: 'date' | 'name' | 'amount') {
  descending.value = sort.value === key ? !descending.value : key !== 'name'
  sort.value = key
}
const avatars = useAdminAvatars(() => visible.value.map((row) => row.user))
</script>

<template>
  <div class="space-y-6">
    <header class="flex items-start justify-between gap-3">
      <div class="min-w-0 flex-1">
        <h1 class="text-2xl font-semibold tracking-tight">{{ t('analytics.title') }}</h1>
        <p class="mt-1 text-sm text-muted">{{ t('analytics.subtitle') }}</p>
      </div>
      <UButton
        icon="i-lucide-refresh-cw"
        color="neutral"
        variant="outline"
        :aria-label="t('adminDashboard.refreshStats')"
        :loading="asyncStatus === 'loading'"
        @click="invalidate(getAnalyticsQueryKey({ query: applied }))"
      />
    </header>
    <form
      class="grid grid-cols-2 sm:flex sm:flex-wrap sm:items-end gap-3"
      @submit.prevent="applyPeriod"
    >
      <div class="col-span-2 flex gap-1 w-full sm:w-auto self-end pb-0.5">
        <UButton
          v-for="days in [7, 30, 90]"
          :key="days"
          :label="t('analytics.days', { days })"
          color="neutral"
          :variant="applied.from === daysAgo(days) && applied.to === today ? 'soft' : 'ghost'"
          @click="preset(days)"
        />
      </div>
      <UFormField :label="t('analytics.from')" class="min-w-0 sm:flex-none">
        <UInput
          v-model="from"
          type="date"
          :max="today"
          :aria-label="t('analytics.from')"
          class="w-full"
        />
      </UFormField>
      <UFormField :label="t('analytics.to')" class="min-w-0 sm:flex-none">
        <UInput
          v-model="to"
          type="date"
          :max="today"
          :aria-label="t('analytics.to')"
          class="w-full"
        />
      </UFormField>
      <UButton
        type="submit"
        :label="t('analytics.apply')"
        color="neutral"
        variant="outline"
        :disabled="!valid"
      />
      <span class="text-xs text-muted self-end pb-2">UTC</span>
    </form>
    <p v-if="!valid" role="alert" class="text-sm text-error">{{ t('analytics.invalidPeriod') }}</p>
    <UAlert
      v-if="error"
      color="error"
      :title="t('analytics.loadFailed')"
      :description="error.message"
    />
    <p v-if="status === 'pending'" role="status" class="py-10 text-muted">
      {{ t('analytics.loading') }}
    </p>
    <template v-else-if="data && summary">
      <div class="grid grid-cols-2 lg:grid-cols-4 border border-default rounded-xl overflow-hidden">
        <button
          v-for="metric in metrics"
          :key="metric.kind"
          type="button"
          class="text-left p-4 sm:p-5 hover:bg-elevated focus-visible:outline-2 focus-visible:outline-primary border-b border-default min-w-0"
          @click="drilldown(metric.kind)"
        >
          <span class="block text-sm text-muted">{{ metric.label }}</span>
          <span class="flex items-center justify-between mt-2 gap-2"
            ><strong class="text-3xl font-semibold tabular-nums">{{ number(metric.value) }}</strong
            ><UIcon name="i-lucide-chevron-right" class="text-muted size-4"
          /></span>
          <span class="block text-xs text-muted mt-1">{{
            metric.value == null ? t('analytics.noData') : metric.hint
          }}</span>
        </button>
      </div>
      <div
        :class="
          ADMIN_NAVIGATION === 'sidebar'
            ? 'space-y-6'
            : 'grid xl:grid-cols-[minmax(0,2fr)_minmax(280px,1fr)] gap-6'
        "
      >
        <section class="min-w-0 rounded-xl border border-default p-4 sm:p-5">
          <div class="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 class="font-semibold text-lg">{{ t('analytics.payments') }}</h2>
              <p class="text-sm text-muted mt-1">
                {{
                  t('analytics.paymentSummary', {
                    payments: summary.payments,
                    buyers: summary.buyers,
                  })
                }}
              </p>
            </div>
            <div class="flex gap-4 text-xs text-muted">
              <span class="flex items-center gap-2"
                ><i class="size-2.5 bg-sky-400 rounded-sm" />{{ t('analytics.firstShort') }}</span
              ><span class="flex items-center gap-2"
                ><i class="size-2.5 bg-violet-400 rounded-sm" />{{
                  t('analytics.repeatShort')
                }}</span
              >
            </div>
          </div>
          <div
            v-if="summary.payments"
            class="flex items-stretch gap-1 h-44 mt-6"
            :aria-label="t('analytics.timeline')"
            role="group"
          >
            <button
              v-for="bin in timeline"
              :key="bin.from"
              type="button"
              class="min-w-0 flex-1 flex flex-col justify-end rounded-sm hover:bg-elevated focus-visible:outline-2 focus-visible:outline-primary"
              :aria-label="`${date(bin.from)}: ${t('analytics.firstShort')} ${bin.first}, ${t('analytics.repeatShort')} ${bin.repeat}`"
              :title="`${date(bin.from)} — ${date(bin.to)}: ${bin.first} / ${bin.repeat}`"
              @click="selectBar(bin)"
            >
              <span
                class="bg-violet-400/85 w-full rounded-t-sm"
                :style="{ height: `${(bin.repeat / chartMax) * 100}%` }"
              />
              <span
                class="bg-sky-400/85 w-full"
                :style="{ height: `${(bin.first / chartMax) * 100}%` }"
              />
              <span v-if="!bin.first && !bin.repeat" class="w-full h-px bg-accented" />
            </button>
          </div>
          <p v-else class="py-12 text-center text-muted">{{ t('analytics.noPayments') }}</p>
          <div class="flex justify-between text-xs text-muted mt-2">
            <span>{{ date(applied.from) }}</span
            ><span>{{
              timeline.length > 0 && timeline[0]?.from !== timeline[0]?.to
                ? t('analytics.weekly')
                : t('analytics.daily')
            }}</span
            ><span>{{ date(applied.to) }}</span>
          </div>
          <p class="text-xs text-muted mt-5">{{ t('analytics.repeatHint') }}</p>
          <button
            type="button"
            class="mt-3 text-sm text-muted underline underline-offset-4 hover:text-default"
            @click="drilldown('credit')"
          >
            {{ t('analytics.creditHint', { count: summary.credit }) }}
          </button>
        </section>
        <section class="rounded-xl border border-default p-4 sm:p-5">
          <h2 class="font-semibold text-lg">{{ t('analytics.currentAccess') }}</h2>
          <p class="text-sm text-muted mt-1">{{ t('analytics.currentHint') }}</p>
          <div
            :class="
              ADMIN_NAVIGATION === 'sidebar'
                ? 'grid sm:grid-cols-3 gap-4 mt-4'
                : 'mt-4 divide-y divide-default'
            "
          >
            <button
              v-for="item in accessCounts"
              :key="item.source"
              type="button"
              class="w-full text-left flex items-center justify-between gap-3 py-3 hover:bg-elevated rounded focus-visible:outline-2 focus-visible:outline-primary"
              @click="drilldown(`current_${item.source}`)"
            >
              <span>{{ sourceLabel(item.source) }}</span
              ><span class="flex items-center gap-2 font-semibold tabular-nums"
                >{{ number(item.count)
                }}<UIcon name="i-lucide-chevron-right" class="size-4 text-muted"
              /></span>
            </button>
          </div>
          <div v-if="paidPlans.length" class="border-t border-default mt-3 pt-3 space-y-1">
            <p class="text-xs text-muted mb-2">{{ t('analytics.paidPlans') }}</p>
            <button
              v-for="[name, count] in paidPlans"
              :key="name"
              type="button"
              class="flex justify-between gap-3 w-full rounded px-2 py-2 text-sm hover:bg-elevated"
              @click="selectPlan(name)"
            >
              <span>{{ name }}</span
              ><span class="tabular-nums">{{ count }}</span>
            </button>
          </div>
        </section>
      </div>
      <div class="grid lg:grid-cols-2 gap-6">
        <section class="border-t border-default pt-5">
          <h2 class="font-semibold text-lg">{{ t('analytics.manualTitle') }}</h2>
          <button
            type="button"
            class="flex items-center gap-4 py-3 text-left hover:underline underline-offset-4"
            @click="drilldown('admin_grant')"
          >
            <strong class="text-2xl tabular-nums">{{ summary.manual }}</strong
            ><span class="text-sm">{{
              t('analytics.manualHint', { users: summary.manualUsers })
            }}</span
            ><UIcon name="i-lucide-chevron-right" class="shrink-0 size-4" />
          </button>
          <p class="text-sm text-muted">{{ t('analytics.manualSeparate') }}</p>
        </section>
        <section class="border-t border-default pt-5">
          <h2 class="font-semibold text-lg">{{ t('analytics.usage') }}</h2>
          <p class="text-sm mt-3" :class="summary.usage === null ? 'text-muted' : ''">
            {{
              summary.usage === null
                ? t('analytics.usageUnavailable')
                : t('analytics.usageSummary', { count: summary.usage.length })
            }}
          </p>
          <p class="text-xs text-muted mt-2">
            {{ t('analytics.usageHint', { threshold: formatTraffic(USAGE_THRESHOLD, true) }) }}
          </p>
        </section>
      </div>
      <section ref="details" class="border-t border-default pt-6 scroll-mt-6">
        <h2 class="text-lg font-semibold">{{ t('analytics.details') }}</h2>
        <div
          class="grid sm:grid-cols-2 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)] gap-3 mt-4"
        >
          <UFormField :label="t('analytics.category')"
            ><USelect
              v-model="selected"
              :items="kindItems"
              :aria-label="t('analytics.category')"
              class="w-full"
          /></UFormField>
          <UFormField :label="t('adminUsers.plan')"
            ><USelect
              v-model="plan"
              :items="planItems"
              :aria-label="t('adminUsers.plan')"
              class="w-full"
          /></UFormField>
          <UFormField :label="t('analytics.user')"
            ><UInput
              v-model="search"
              :placeholder="t('adminUsers.recipientSearch')"
              :aria-label="t('analytics.user')"
              class="w-full"
          /></UFormField>
        </div>
        <div class="flex flex-wrap items-center justify-between gap-3 my-4 text-sm text-muted">
          <span>{{ t('adminWorkspace.found', { count: filtered.length }) }}</span>
          <UButton
            v-if="windowFilter"
            color="neutral"
            variant="soft"
            icon="i-lucide-x"
            :label="`${date(windowFilter.from)} — ${date(windowFilter.to)}`"
            @click="windowFilter = null"
          />
          <div class="flex gap-1 md:hidden">
            <USelect
              v-model="sort"
              :items="[
                { value: 'date', label: t('analytics.date') },
                { value: 'name', label: t('analytics.user') },
                { value: 'amount', label: t('analytics.amount') },
              ]"
              :aria-label="t('adminWorkspace.sort')"
            /><UButton
              color="neutral"
              variant="outline"
              :icon="descending ? 'i-lucide-arrow-down' : 'i-lucide-arrow-up'"
              :aria-label="t('analytics.reverseSort')"
              @click="descending = !descending"
            />
          </div>
        </div>
        <p v-if="selected === 'usage'" class="text-xs text-muted mb-3">
          {{ t('analytics.usageSourceHint') }}
        </p>
        <p v-if="selected === 'usage' && summary.usage === null" class="py-8 text-muted">
          {{ t('analytics.usageUnavailable') }}
        </p>
        <p v-else-if="!filtered.length" class="py-8 text-muted">{{ t('analytics.noEvents') }}</p>
        <template v-else>
          <table class="hidden md:table w-full text-sm text-left">
            <thead class="text-muted border-b border-default">
              <tr>
                <th class="py-3">
                  <button type="button" @click="changeSort('name')">
                    {{ t('analytics.user') }} {{ sort === 'name' ? (descending ? '↓' : '↑') : '↕' }}
                  </button>
                </th>
                <th class="py-3">{{ t('adminUsers.plan') }}</th>
                <th class="py-3">{{ t('analytics.category') }}</th>
                <th class="py-3">
                  <button type="button" @click="changeSort('date')">
                    {{ t('analytics.date') }} {{ sort === 'date' ? (descending ? '↓' : '↑') : '↕' }}
                  </button>
                </th>
                <th class="py-3 text-right">
                  <button type="button" @click="changeSort('amount')">
                    {{ selected === 'usage' ? t('analytics.traffic') : t('analytics.amount') }}
                    {{ sort === 'amount' ? (descending ? '↓' : '↑') : '↕' }}
                  </button>
                </th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="row in visible"
                :key="row.id"
                class="border-b border-default hover:bg-elevated"
              >
                <td class="py-4 pr-3">
                  <RouterLink
                    :to="`/admin/users/${row.user}`"
                    class="font-medium underline underline-offset-4"
                    ><AdminUserIdentity
                      :name="row.name"
                      :avatar="avatars[String(row.user)]" /></RouterLink
                  ><span class="block text-xs text-muted mt-1">ID {{ row.user }}</span>
                </td>
                <td class="py-4 pr-3">{{ row.plan }}</td>
                <td class="py-4 pr-3">{{ sourceLabel(row.kind) }}</td>
                <td class="py-4 whitespace-nowrap">{{ row.date ? date(row.date) : '—' }}</td>
                <td class="py-4 text-right tabular-nums">
                  {{
                    row.traffic != null
                      ? formatTraffic(row.traffic, true)
                      : row.kind === 'first' || row.kind === 'repeat'
                        ? `${number(row.amount)} ⭐`
                        : '—'
                  }}
                </td>
              </tr>
            </tbody>
          </table>
          <ul class="md:hidden divide-y divide-default">
            <li v-for="row in visible" :key="row.id" class="py-4">
              <RouterLink
                :to="`/admin/users/${row.user}`"
                class="font-medium underline underline-offset-4 break-words"
                ><AdminUserIdentity :name="row.name" :avatar="avatars[String(row.user)]"
              /></RouterLink>
              <p class="text-xs text-muted mt-1">ID {{ row.user }} · {{ row.plan }}</p>
              <div class="flex flex-wrap justify-between gap-2 mt-2 text-sm">
                <span>{{ sourceLabel(row.kind) }}</span
                ><span>{{
                  row.traffic != null
                    ? formatTraffic(row.traffic, true)
                    : row.kind === 'first' || row.kind === 'repeat'
                      ? `${number(row.amount)} ⭐`
                      : row.date
                        ? date(row.date)
                        : ''
                }}</span>
              </div>
              <p
                v-if="row.date && (row.kind === 'first' || row.kind === 'repeat')"
                class="text-xs text-muted mt-1"
              >
                {{ date(row.date) }}
              </p>
            </li>
          </ul>
          <UPagination
            v-if="filtered.length > 10"
            v-model:page="page"
            :total="filtered.length"
            :items-per-page="10"
            :sibling-count="0"
            :show-edges="false"
            class="mt-4"
          />
        </template>
      </section>
    </template>
  </div>
</template>
