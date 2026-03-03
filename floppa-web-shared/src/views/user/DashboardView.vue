<script setup lang="ts">
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getMeQuery, getMyPeersQuery } from '../../client/@pinia/colada.gen'
import { useAuthStore } from '../../stores'
import { formatBytes } from '../../utils'
import StatsCard from '../../components/StatsCard.vue'

const router = useRouter()
const { t } = useI18n()
const auth = useAuthStore()
const { data: me, status: meStatus, error: meError } = useQuery(getMeQuery())
const { data: peers, status: peersStatus, error: peersError } = useQuery(getMyPeersQuery())

const loading = computed(() => meStatus.value === 'pending' || peersStatus.value === 'pending')
const error = computed(() => meError.value || peersError.value)

function formatDate(date: string): string {
  return new Date(date).toLocaleDateString()
}

const totalTraffic = computed(() => {
  if (!peers.value) return 0
  return peers.value.reduce((sum, p) => sum + p.tx_bytes + p.rx_bytes, 0)
})

const hasSubscription = computed(() => !!me.value?.subscription)

const isPermanent = computed(() => {
  return me.value?.subscription?.expires_at === null
})

const daysRemaining = computed(() => {
  if (!me.value?.subscription?.expires_at) return 0
  const expires = new Date(me.value.subscription.expires_at)
  const now = new Date()
  return Math.max(0, Math.ceil((expires.getTime() - now.getTime()) / (1000 * 60 * 60 * 24)))
})
</script>

<template>
  <div class="max-w-3xl mx-auto">
    <h1 class="hidden md:block text-2xl font-bold mb-6">{{ t('userDashboard.title') }}</h1>

    <slot name="vpn-widget" />

    <div v-if="loading" class="flex justify-center py-12">
      <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
    </div>
    <UAlert v-else-if="error" color="error" :title="error.message" />
    <template v-else-if="me">
      <!-- User Info -->
      <UCard class="mb-4 bg-gradient-to-br from-[var(--ui-primary)] to-[var(--ui-primary)]/80">
        <div class="flex items-center gap-4 text-white">
          <UAvatar
            :src="auth.avatarUrl ?? me.photo_url ?? undefined"
            :alt="me.first_name ?? me.username ?? undefined"
            icon="i-lucide-user"
            size="lg"
          />
          <div class="min-w-0">
            <p class="text-sm opacity-85">{{ t('userDashboard.welcome') }}</p>
            <h2 class="text-xl font-bold text-white break-words">
              {{ me.first_name ? (me.last_name ? `${me.first_name} ${me.last_name}` : me.first_name) : (me.username || `User #${me.id}`) }}
            </h2>
          </div>
        </div>
      </UCard>

      <!-- Subscription Status -->
      <UCard class="mb-6">
        <template #header>
          <div class="flex items-center gap-2 font-semibold">
            <UIcon name="i-lucide-ticket" />
            {{ t('userDashboard.subscription') }}
          </div>
        </template>

        <div v-if="hasSubscription">
          <div class="flex items-center gap-3 mb-4">
            <span class="text-xl font-semibold">{{ me.subscription!.plan_display_name }}</span>
            <UBadge color="success" :label="t('status.active')" />
          </div>
          <div class="flex flex-col gap-2">
            <div class="flex items-center gap-2 text-[var(--ui-text-muted)]">
              <UIcon name="i-lucide-calendar" class="size-4" />
              <span v-if="isPermanent">{{ t('userDashboard.permanentLabel') }}</span>
              <span v-else>{{ t('userDashboard.expires', { date: formatDate(me.subscription!.expires_at!), days: daysRemaining }) }}</span>
            </div>
            <div class="flex items-center gap-2 text-[var(--ui-text-muted)]">
              <UIcon name="i-lucide-zap" class="size-4" />
              <span>{{ t('userDashboard.speed', { speed: me.subscription!.speed_limit_mbps ? `${me.subscription!.speed_limit_mbps} Mbps` : t('common.unlimited') }) }}</span>
            </div>
            <div class="flex items-center gap-2 text-[var(--ui-text-muted)]">
              <UIcon name="i-lucide-key" class="size-4" />
              <span>{{ t('userDashboard.maxConfigs', { count: me.subscription!.max_peers }) }}</span>
            </div>
          </div>
        </div>
        <div v-else class="flex flex-col items-center gap-2 py-4 text-center text-[var(--ui-text-muted)]">
          <UIcon name="i-lucide-info" class="text-3xl text-yellow-500" />
          <p>{{ t('userDashboard.noSubscription') }}</p>
          <p class="text-sm">{{ t('userDashboard.contactSupport') }}</p>
        </div>
      </UCard>

      <!-- Quick Stats -->
      <div class="mb-6">
        <h3 class="flex items-center gap-2 font-semibold mb-4">
          <UIcon name="i-lucide-bar-chart-3" />
          {{ t('userDashboard.usage') }}
        </h3>
        <div class="grid grid-cols-[repeat(auto-fit,minmax(150px,1fr))] gap-4">
          <StatsCard :label="t('userDashboard.activeConfigs')" :value="peers?.length ?? 0" icon="i-lucide-key" />
          <StatsCard :label="t('userDashboard.totalTraffic')" :value="formatBytes(totalTraffic)" icon="i-lucide-cloud" />
        </div>
      </div>

      <!-- Quick Actions -->
      <div class="flex gap-3 flex-wrap">
        <UButton :label="t('userDashboard.manageConfigs')" icon="i-lucide-settings" @click="router.push('/peers')" />
        <UButton
          v-if="auth.isAdmin"
          :label="t('userDashboard.adminPanel')"
          icon="i-lucide-shield"
          color="neutral"
          variant="outline"
          @click="router.push('/admin')"
        />
      </div>
    </template>
  </div>
</template>
