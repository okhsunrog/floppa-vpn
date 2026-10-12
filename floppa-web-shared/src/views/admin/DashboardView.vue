<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import {
  getStatsQuery,
  getStatsQueryKey,
  listUsersQuery,
  listUsersQueryKey,
  listPlansQuery,
  listPlansQueryKey,
} from '../../client/@pinia/colada.gen'
import { formatTraffic, formatDate } from '../../utils'
import { useInvalidateQueries } from '../../composables/invalidate'
import { selectAdminUsers } from '../../composables/adminUsers'
const { t, locale } = useI18n()
const { data: stats, asyncStatus, error: statsError } = useQuery(getStatsQuery())
const { data: users, error: usersError } = useQuery(listUsersQuery())
const { data: plans, error: plansError } = useQuery(listPlansQuery())
const invalidate = useInvalidateQueries()
const refresh = () => invalidate(getStatsQueryKey(), listUsersQueryKey(), listPlansQueryKey())
const expiring = computed(() =>
  selectAdminUsers(users.value ?? [], {
    plan: 'all',
    subscription: 'active',
    expiring: true,
    sort: 'subscription_expires_at',
    descending: false,
    now: Date.now(),
    locale: locale.value,
  }),
)
const expired = computed(
  () =>
    users.value?.filter((user) => user.current_plan_id != null && !user.subscription_active).length,
)
const distribution = computed(() =>
  (plans.value ?? [])
    .map((plan) => ({
      ...plan,
      count: (users.value ?? []).filter(
        (user) => user.subscription_active && user.current_plan_id === plan.id,
      ).length,
    }))
    .sort((a, b) => b.count - a.count || a.id - b.id),
)
const activeCount = computed(() => users.value?.filter((user) => user.subscription_active).length)
const metrics = computed(() => [
  {
    title: t('adminWorkspace.allUsers'),
    count: users.value?.length,
    to: '/admin/users',
    icon: 'i-lucide-users',
  },
  {
    title: t('adminDashboard.currentSubscriptions'),
    count: activeCount.value,
    to: '/admin/subscriptions',
    icon: 'i-lucide-ticket',
  },
  {
    title: t('adminWorkspace.expiredHint'),
    count: expired.value,
    to: '/admin/subscriptions?subscription=expired',
    icon: 'i-lucide-calendar-clock',
  },
])
</script>

<template>
  <div class="space-y-7">
    <div class="flex items-start justify-between gap-4">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">{{ t('adminDashboard.title') }}</h1>
        <p class="text-sm text-[var(--ui-text-muted)] mt-1">
          {{ t('adminWorkspace.overviewHint') }}
        </p>
      </div>
      <UButton
        icon="i-lucide-refresh-cw"
        color="neutral"
        variant="outline"
        size="lg"
        :aria-label="t('adminDashboard.refreshStats')"
        :loading="asyncStatus === 'loading'"
        @click="refresh"
      />
    </div>
    <UAlert
      v-if="usersError || plansError || statsError"
      color="error"
      :title="(usersError || plansError || statsError)?.message"
    />

    <div
      class="grid sm:grid-cols-3 rounded-xl border border-[var(--ui-border)] divide-y sm:divide-y-0 sm:divide-x divide-[var(--ui-border)] overflow-hidden"
    >
      <RouterLink
        v-for="metric in metrics"
        :key="metric.to"
        :to="metric.to"
        class="group flex items-center justify-between gap-4 p-5 hover:bg-[var(--ui-bg-elevated)] focus-visible:outline-2 focus-visible:outline-inset focus-visible:outline-[var(--ui-primary)]"
      >
        <div class="min-w-0">
          <span class="block text-sm text-[var(--ui-text-muted)]">{{ metric.title }}</span
          ><span class="block text-3xl font-semibold tabular-nums mt-1">{{
            metric.count ?? '—'
          }}</span>
        </div>
        <UIcon
          name="i-lucide-chevron-right"
          class="size-5 shrink-0 text-[var(--ui-text-muted)] group-hover:text-[var(--ui-text)]"
        />
      </RouterLink>
    </div>

    <div class="grid xl:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] gap-6 items-start">
      <section class="min-w-0 rounded-xl border border-[var(--ui-border)] overflow-hidden">
        <div class="p-5 border-b border-[var(--ui-border)]">
          <div class="flex justify-between items-center gap-3">
            <h2 class="font-semibold text-lg">{{ t('adminWorkspace.expiresSoon') }}</h2>
            <UBadge
              color="neutral"
              variant="subtle"
              :label="users ? String(expiring.length) : '—'"
            />
          </div>
          <p class="text-sm text-[var(--ui-text-muted)] mt-1">{{ t('adminWorkspace.expiring') }}</p>
        </div>
        <p v-if="!users" class="p-5 text-sm text-[var(--ui-text-muted)]">
          {{ usersError ? usersError.message : t('common.loading') }}
        </p>
        <p v-else-if="!expiring.length" class="p-5 text-sm text-[var(--ui-text-muted)]">
          {{ t('adminWorkspace.noExpiring') }}
        </p>
        <ul v-else class="divide-y divide-[var(--ui-border)]">
          <li v-for="user in expiring.slice(0, 5)" :key="user.id">
            <RouterLink
              :to="`/admin/users/${user.id}`"
              class="flex items-center justify-between gap-4 p-5 hover:bg-[var(--ui-bg-elevated)] focus-visible:outline-2 focus-visible:outline-inset focus-visible:outline-[var(--ui-primary)]"
            >
              <div class="min-w-0">
                <p class="font-medium break-words">
                  {{
                    [user.first_name, user.last_name].filter(Boolean).join(' ') ||
                    user.username ||
                    `#${user.id}`
                  }}
                </p>
                <p class="text-sm text-[var(--ui-text-muted)] mt-1 break-words">
                  {{ user.current_plan }}
                </p>
              </div>
              <div class="flex items-center gap-2 shrink-0 text-sm">
                <time :datetime="user.subscription_expires_at ?? undefined">{{
                  formatDate(user.subscription_expires_at!)
                }}</time
                ><UIcon name="i-lucide-chevron-right" />
              </div>
            </RouterLink>
          </li>
        </ul>
        <div class="p-4 border-t border-[var(--ui-border)]">
          <UButton
            to="/admin/subscriptions?expires=week"
            :label="t('adminWorkspace.openList')"
            color="neutral"
            variant="outline"
            block
          />
        </div>
      </section>
      <section class="min-w-0 rounded-xl border border-[var(--ui-border)] overflow-hidden">
        <h2 class="p-5 font-semibold text-lg border-b border-[var(--ui-border)]">
          {{ t('adminWorkspace.plansTitle') }}
        </h2>
        <p v-if="!plans || !users" class="p-5 text-sm text-[var(--ui-text-muted)]">
          {{ (plansError || usersError)?.message || t('common.loading') }}
        </p>
        <p v-else-if="!distribution.length" class="p-5 text-sm text-[var(--ui-text-muted)]">
          {{ t('adminWorkspace.noSubscriptions') }}
        </p>
        <ul v-else class="p-2">
          <li v-for="plan in distribution" :key="plan.id">
            <RouterLink
              :to="`/admin/subscriptions?plan=${plan.id}`"
              class="block p-3 rounded-lg hover:bg-[var(--ui-bg-elevated)] focus-visible:outline-2 focus-visible:outline-[var(--ui-primary)]"
            >
              <div class="flex justify-between gap-3 text-sm">
                <span class="min-w-0 break-words">{{ plan.display_name }}</span
                ><span class="flex items-center gap-2 tabular-nums font-medium"
                  >{{ plan.count
                  }}<UIcon name="i-lucide-chevron-right" class="text-[var(--ui-text-muted)]"
                /></span>
              </div>
              <div
                class="h-1.5 rounded mt-2 bg-[var(--ui-bg-elevated)] overflow-hidden"
                aria-hidden="true"
              >
                <div
                  class="h-full rounded bg-[var(--ui-text-dimmed)]"
                  :style="{ width: `${activeCount ? (plan.count / activeCount) * 100 : 0}%` }"
                />
              </div>
            </RouterLink>
          </li>
        </ul>
        <div class="p-4 border-t border-[var(--ui-border)]">
          <UButton
            to="/admin/plans"
            :label="t('adminWorkspace.managePlans')"
            color="neutral"
            variant="outline"
            block
          />
        </div>
      </section>
    </div>

    <div class="grid lg:grid-cols-2 gap-6">
      <section class="border-t border-[var(--ui-border)] pt-5">
        <h2 class="font-semibold mb-3">{{ t('adminDashboard.management') }}</h2>
        <div class="flex flex-wrap gap-2">
          <UButton
            to="/admin/peers"
            :label="t('nav.peers')"
            icon="i-lucide-link"
            color="neutral"
            variant="outline"
          />
          <UButton
            to="/admin/vless"
            label="VLESS"
            icon="i-lucide-globe"
            color="neutral"
            variant="outline"
          />
          <UButton
            to="/admin/installations"
            :label="t('nav.installations')"
            icon="i-lucide-monitor-smartphone"
            color="neutral"
            variant="outline"
          />
        </div>
        <p class="text-sm text-[var(--ui-text-muted)] mt-3">
          {{ t('adminDashboard.configs') }}: {{ stats?.active_peers ?? '—' }}.
          {{ t('adminDashboard.configsHint') }}
        </p>
      </section>
      <section class="border-t border-[var(--ui-border)] pt-5">
        <h2 class="font-semibold mb-3">{{ t('adminDashboard.trafficPeriod') }}</h2>
        <dl class="grid grid-cols-2 gap-4">
          <div>
            <dt class="text-sm text-[var(--ui-text-muted)]">{{ t('adminDashboard.download') }}</dt>
            <dd class="text-xl tabular-nums mt-1">
              {{ stats ? formatTraffic(stats.total_download_bytes, stats.traffic_available) : '—' }}
            </dd>
          </div>
          <div>
            <dt class="text-sm text-[var(--ui-text-muted)]">{{ t('adminDashboard.upload') }}</dt>
            <dd class="text-xl tabular-nums mt-1">
              {{ stats ? formatTraffic(stats.total_upload_bytes, stats.traffic_available) : '—' }}
            </dd>
          </div>
        </dl>
        <p
          v-if="stats && !stats.traffic_available"
          class="text-sm text-[var(--ui-text-muted)] mt-3"
        >
          {{ t('traffic.unavailable') }}
        </p>
      </section>
    </div>
  </div>
</template>
