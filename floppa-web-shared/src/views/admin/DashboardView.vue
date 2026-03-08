<script setup lang="ts">
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getStatsQuery } from '../../client/@pinia/colada.gen'
import { formatBytes } from '../../utils'
import StatsCard from '../../components/StatsCard.vue'

const router = useRouter()
const { t } = useI18n()
const { data: stats, status, error } = useQuery(getStatsQuery())
</script>

<template>
  <div class="max-w-5xl mx-auto">
    <h1 class="text-2xl font-bold mb-6">{{ t('adminDashboard.title') }}</h1>

    <div v-if="status === 'pending'" class="flex justify-center py-12">
      <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
    </div>
    <UAlert v-else-if="error" color="error" :title="error.message" />
    <template v-else-if="stats">
      <div class="grid grid-cols-[repeat(auto-fit,minmax(180px,1fr))] gap-4 mb-8">
        <StatsCard
          :label="t('adminDashboard.totalUsers')"
          :value="stats.total_users"
          icon="i-lucide-users"
          large
        />
        <StatsCard
          :label="t('adminDashboard.activePeers')"
          :value="stats.active_peers"
          icon="i-lucide-link"
          large
        />
        <StatsCard
          :label="t('adminDashboard.subscriptions')"
          :value="stats.active_subscriptions"
          icon="i-lucide-ticket"
          large
        />
        <StatsCard
          :label="t('adminDashboard.totalDownload')"
          :value="formatBytes(stats.total_download_bytes)"
          icon="i-lucide-arrow-down"
          large
        />
        <StatsCard
          :label="t('adminDashboard.totalUpload')"
          :value="formatBytes(stats.total_upload_bytes)"
          icon="i-lucide-arrow-up"
          large
        />
      </div>

      <div class="grid grid-cols-[repeat(auto-fit,minmax(250px,1fr))] gap-4">
        <UCard
          class="cursor-pointer hover:-translate-y-0.5 transition-transform"
          @click="router.push('/admin/users')"
        >
          <div class="flex items-center gap-4">
            <UIcon name="i-lucide-users" class="text-3xl text-[var(--ui-primary)]" />
            <div>
              <span class="block text-lg font-semibold">{{ t('adminDashboard.users') }}</span>
              <span class="block text-sm text-[var(--ui-text-muted)]">{{
                t('adminDashboard.usersDescription')
              }}</span>
            </div>
          </div>
        </UCard>
        <UCard
          class="cursor-pointer hover:-translate-y-0.5 transition-transform"
          @click="router.push('/admin/plans')"
        >
          <div class="flex items-center gap-4">
            <UIcon name="i-lucide-list" class="text-3xl text-[var(--ui-primary)]" />
            <div>
              <span class="block text-lg font-semibold">{{ t('adminDashboard.plans') }}</span>
              <span class="block text-sm text-[var(--ui-text-muted)]">{{
                t('adminDashboard.plansDescription')
              }}</span>
            </div>
          </div>
        </UCard>
      </div>
    </template>
  </div>
</template>
