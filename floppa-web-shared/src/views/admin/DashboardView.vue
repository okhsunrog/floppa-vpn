<script setup lang="ts">
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import {
  getStatsQuery,
  getStatsQueryKey,
  listUsersQueryKey,
  listPeersQueryKey,
} from '../../client/@pinia/colada.gen'
import { formatTraffic } from '../../utils'
import { useInvalidateQueries } from '../../composables/invalidate'
import UsersView from './UsersView.vue'
import PeersView from './PeersView.vue'

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const { data: stats, asyncStatus, error } = useQuery(getStatsQuery())
const invalidate = useInvalidateQueries()
const refresh = () => invalidate(getStatsQueryKey(), listUsersQueryKey(), listPeersQueryKey())
const view = computed(() =>
  ['subscriptions', 'configs'].includes(String(route.query.view))
    ? String(route.query.view)
    : 'users',
)
const cards = computed(() => [
  {
    id: 'users',
    icon: 'i-lucide-users',
    title: t('adminDashboard.users'),
    count: stats.value?.total_users,
    hint: t('adminDashboard.allUsersHint'),
    action: t('adminDashboard.showUsers'),
  },
  {
    id: 'subscriptions',
    icon: 'i-lucide-ticket',
    title: t('adminDashboard.currentSubscriptions'),
    count: stats.value?.active_subscriptions,
    hint: t('adminDashboard.subscriptionsHint'),
    action: t('adminDashboard.showSubscriptions'),
  },
  {
    id: 'configs',
    icon: 'i-lucide-key-round',
    title: t('adminDashboard.configs'),
    count: stats.value?.active_peers,
    hint: t('adminDashboard.configsHint'),
    action: t('adminDashboard.showConfigs'),
  },
])
function selectView(id: string) {
  void router.push({ query: { ...route.query, view: id === 'users' ? undefined : id } })
}
</script>

<template>
  <div class="max-w-7xl mx-auto space-y-6">
    <div class="flex items-start justify-between gap-4">
      <div>
        <h1 class="text-2xl font-bold">{{ t('adminDashboard.title') }}</h1>
        <p class="text-sm text-[var(--ui-text-muted)] mt-1">
          {{ t('adminDashboard.overviewHint') }}
        </p>
      </div>
      <UButton
        icon="i-lucide-refresh-cw"
        color="neutral"
        variant="ghost"
        :aria-label="t('adminDashboard.refreshStats')"
        :loading="asyncStatus === 'loading'"
        @click="refresh()"
      />
    </div>

    <UAlert v-if="error" color="error" :title="error.message" />
    <div class="grid grid-cols-1 sm:grid-cols-3 gap-3" aria-controls="admin-overview-list">
      <button
        v-for="card in cards"
        :key="card.id"
        type="button"
        :aria-pressed="view === card.id"
        @click="selectView(card.id)"
        class="grid grid-cols-[1fr_auto] sm:block text-left rounded-xl border p-4 sm:p-5 transition-colors focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-[var(--ui-primary)]"
        :class="
          view === card.id
            ? 'border-[var(--ui-primary)] bg-[var(--ui-bg-elevated)]'
            : 'border-[var(--ui-border)] hover:border-[var(--ui-primary)] hover:bg-[var(--ui-bg-elevated)]'
        "
      >
        <div class="flex items-center justify-between gap-2">
          <span class="font-medium">{{ card.title }}</span>
          <UIcon :name="card.icon" class="hidden sm:block size-5 text-[var(--ui-primary)]" />
        </div>
        <div
          class="text-3xl font-semibold sm:mt-3 tabular-nums row-span-2 row-start-1 col-start-2 sm:row-auto sm:col-auto"
        >
          {{ card.count ?? '—' }}
        </div>
        <p class="text-sm text-[var(--ui-text-muted)] mt-2 col-start-1">{{ card.hint }}</p>
        <div
          class="flex items-center justify-between gap-2 mt-3 sm:mt-4 text-sm text-[var(--ui-primary)] col-span-2"
        >
          <span>{{ card.action }}</span>
          <UIcon
            :name="view === card.id ? 'i-lucide-check' : 'i-lucide-arrow-down'"
            class="size-4"
          />
        </div>
      </button>
    </div>

    <section
      id="admin-overview-list"
      class="border border-[var(--ui-border)] rounded-xl p-4 sm:p-6"
    >
      <div v-if="view !== 'users'" class="flex items-center justify-between flex-wrap gap-2 mb-5">
        <UBadge
          color="primary"
          variant="subtle"
          :label="
            t(
              view === 'subscriptions'
                ? 'adminDashboard.subscriptionFilter'
                : 'adminDashboard.configFilter',
            )
          "
        />
        <UButton
          :label="t('adminDashboard.clearFilter')"
          icon="i-lucide-x"
          color="neutral"
          variant="ghost"
          size="sm"
          @click="selectView('users')"
        />
      </div>
      <PeersView v-if="view === 'configs'" embedded active-only />
      <UsersView v-else embedded :active-only="view === 'subscriptions'" />
    </section>

    <div class="grid grid-cols-1 lg:grid-cols-[1fr_1fr] gap-4">
      <section class="border border-[var(--ui-border)] rounded-xl p-5">
        <h2 class="font-semibold">{{ t('adminDashboard.trafficPeriod') }}</h2>
        <div class="grid grid-cols-2 gap-4 mt-4">
          <div>
            <p class="text-sm text-[var(--ui-text-muted)] flex items-center gap-2">
              <UIcon name="i-lucide-arrow-down" />{{ t('adminDashboard.download') }}
            </p>
            <p class="text-xl font-semibold mt-1">
              {{ stats ? formatTraffic(stats.total_download_bytes, stats.traffic_available) : '—' }}
            </p>
          </div>
          <div>
            <p class="text-sm text-[var(--ui-text-muted)] flex items-center gap-2">
              <UIcon name="i-lucide-arrow-up" />{{ t('adminDashboard.upload') }}
            </p>
            <p class="text-xl font-semibold mt-1">
              {{ stats ? formatTraffic(stats.total_upload_bytes, stats.traffic_available) : '—' }}
            </p>
          </div>
        </div>
        <p
          v-if="stats && !stats.traffic_available"
          class="text-sm text-[var(--ui-text-muted)] mt-3"
        >
          {{ t('traffic.unavailable') }}
        </p>
      </section>
      <section class="border border-[var(--ui-border)] rounded-xl p-5">
        <h2 class="font-semibold mb-3">{{ t('adminDashboard.management') }}</h2>
        <div class="flex flex-col items-start gap-2">
          <UButton
            to="/admin/plans"
            :label="t('adminDashboard.plansAccess')"
            icon="i-lucide-list"
            color="neutral"
            variant="link"
          />
          <UButton
            to="/admin/installations"
            :label="t('adminDashboard.installations')"
            icon="i-lucide-monitor-smartphone"
            color="neutral"
            variant="link"
          />
          <UButton
            to="/admin/vless"
            label="VLESS"
            icon="i-lucide-shield"
            color="neutral"
            variant="link"
          />
        </div>
      </section>
    </div>
  </div>
</template>
