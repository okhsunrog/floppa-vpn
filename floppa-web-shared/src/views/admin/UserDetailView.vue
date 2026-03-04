<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import {
  getUserQuery,
  listPlansQuery,
  setSubscriptionMutation,
  deleteSubscriptionMutation,
  deleteAdminPeerMutation,
} from '../../client/@pinia/colada.gen'
import { formatBytes } from '../../utils'
import StatusBadge from '../../components/StatusBadge.vue'
import type { PeerSyncStatus } from '../../types'

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const toast = useToast()

const userId = Number(route.params.id)

const {
  data: user,
  status: userStatus,
  error: userError,
  refresh: refreshUser,
} = useQuery(getUserQuery({ path: { id: userId } }))
const { data: plans, status: plansStatus, error: plansError } = useQuery(listPlansQuery())

const loading = computed(() => userStatus.value === 'pending' || plansStatus.value === 'pending')
const error = computed(() => userError.value || plansError.value)

const setSubMut = useMutation(setSubscriptionMutation())
const deleteSubMut = useMutation(deleteSubscriptionMutation())
const deletePeerMut = useMutation(deleteAdminPeerMutation())

const submitting = computed(
  () =>
    setSubMut.asyncStatus.value === 'loading' ||
    deleteSubMut.asyncStatus.value === 'loading' ||
    deletePeerMut.asyncStatus.value === 'loading',
)

// Subscription dialog (set/change)
const subDialog = ref(false)
const subPlanId = ref<number | undefined>(undefined)
const subDays = ref(30)
const subPermanent = ref(false)

// Delete subscription confirm
const deleteSubConfirm = ref(false)

// Confirm remove peer
const confirmOpen = ref(false)
const confirmMessage = ref('')
const pendingPeerId = ref<number | null>(null)

// Show subscription history
const showHistory = ref(false)

// Set default plan when plans load
watch(plans, (plansData) => {
  if (plansData && plansData.length > 0 && subPlanId.value === undefined) {
    const standardPlan = plansData.find((p) => p.name === 'standard')
    subPlanId.value = standardPlan?.id ?? plansData[0]?.id
  }
})

// Auto-fill days from trial_days when a trial plan is selected
watch(subPlanId, (planId) => {
  if (!planId || !plans.value) return
  const plan = plans.value.find((p) => p.id === planId)
  if (plan?.trial_days) {
    subDays.value = plan.trial_days
  }
})

const planItems = computed(() =>
  (plans.value ?? []).map((p) => ({ label: p.display_name, value: p.id })),
)

const activeSubscription = computed(() => {
  if (!user.value) return null
  return user.value.subscriptions.find((s) => s.is_active) ?? null
})

const historySubscriptions = computed(() => {
  if (!user.value) return []
  return user.value.subscriptions.filter((s) => !s.is_active)
})

function formatDate(date: string): string {
  return new Date(date).toLocaleString()
}

function formatLimit(mbps: number | null | undefined): string {
  return mbps == null ? t('common.unlimited') : `${mbps} Mbps`
}

function formatTrafficLimit(bytes: number | null | undefined): string {
  if (bytes == null) return t('common.unlimited')
  const gb = bytes / (1024 * 1024 * 1024)
  return `${gb.toFixed(1)} GB`
}

function openSetSubDialog() {
  // Pre-fill with current subscription's plan if exists
  if (activeSubscription.value) {
    subPlanId.value = activeSubscription.value.plan_id
    subPermanent.value = !activeSubscription.value.expires_at
  } else {
    subPermanent.value = false
  }
  subDays.value = 30
  subDialog.value = true
}

async function setSubscription() {
  if (!user.value || !subPlanId.value) return
  try {
    await setSubMut.mutateAsync({
      path: { id: user.value.id },
      body: {
        plan_id: subPlanId.value,
        days: subPermanent.value ? undefined : subDays.value,
        permanent: subPermanent.value,
      },
    })
    await refreshUser()
    subDialog.value = false
    toast.add({
      title: t('common.success'),
      description: t('adminUserDetail.subscriptionChanged'),
      color: 'success',
    })
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: e instanceof Error ? e.message : t('adminUserDetail.changeFailed'),
      color: 'error',
    })
  }
}

async function deleteSubscription() {
  if (!user.value) return
  try {
    await deleteSubMut.mutateAsync({ path: { id: user.value.id } })
    await refreshUser()
    deleteSubConfirm.value = false
    toast.add({
      title: t('common.success'),
      description: t('adminUserDetail.subscriptionDeleted'),
      color: 'success',
    })
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: e instanceof Error ? e.message : t('adminUserDetail.deleteFailed'),
      color: 'error',
    })
  }
}

function confirmRemovePeer(peerId: number, peerIp: string) {
  pendingPeerId.value = peerId
  confirmMessage.value = t('adminUserDetail.removePeerConfirm', { ip: peerIp })
  confirmOpen.value = true
}

async function doRemovePeer() {
  if (!user.value || !pendingPeerId.value) return
  try {
    await deletePeerMut.mutateAsync({ path: { id: pendingPeerId.value } })
    await refreshUser()
    toast.add({
      title: t('common.success'),
      description: t('adminUserDetail.peerRemoved'),
      color: 'success',
    })
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: e instanceof Error ? e.message : t('adminUserDetail.removeFailed'),
      color: 'error',
    })
  }
  confirmOpen.value = false
  pendingPeerId.value = null
}
</script>

<template>
  <div class="max-w-4xl mx-auto">
    <UButton
      :label="t('adminUserDetail.backToUsers')"
      icon="i-lucide-arrow-left"
      color="neutral"
      variant="ghost"
      class="mb-4"
      @click="router.push('/admin/users')"
    />

    <div v-if="loading" class="flex justify-center py-12">
      <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
    </div>
    <UAlert v-else-if="error" color="error" :title="error.message" />
    <template v-else-if="user">
      <div class="flex items-center gap-4 mb-6">
        <UAvatar
          :src="user.photo_url ?? undefined"
          :alt="user.first_name ?? user.username ?? undefined"
          icon="i-lucide-user"
          size="xl"
        />
        <div>
          <div class="flex items-center gap-3">
            <h1 class="text-2xl font-bold">
              {{
                user.first_name
                  ? user.last_name
                    ? `${user.first_name} ${user.last_name}`
                    : user.first_name
                  : user.username || `User #${user.id}`
              }}
            </h1>
            <UBadge
              :color="user.is_admin ? 'info' : 'neutral'"
              :label="user.is_admin ? t('common.admin') : t('common.user')"
              variant="subtle"
            />
          </div>
          <a
            v-if="user.username"
            :href="`https://t.me/${user.username}`"
            target="_blank"
            class="text-sm text-[var(--ui-text-muted)] hover:text-[var(--ui-primary)] underline"
            >@{{ user.username }}</a
          >
        </div>
      </div>

      <!-- User Info Card -->
      <UCard class="mb-6">
        <div class="grid grid-cols-[repeat(auto-fit,minmax(150px,1fr))] gap-6">
          <div class="flex flex-col gap-1">
            <span class="text-xs text-[var(--ui-text-muted)] uppercase">{{
              t('adminUserDetail.id')
            }}</span>
            <span class="font-medium">{{ user.id }}</span>
          </div>
          <div class="flex flex-col gap-1">
            <span class="text-xs text-[var(--ui-text-muted)] uppercase">{{
              t('adminUserDetail.telegramId')
            }}</span>
            <a
              v-if="user.username"
              :href="`https://t.me/${user.username}`"
              target="_blank"
              class="font-medium text-[var(--ui-primary)] hover:underline"
              >{{ user.telegram_id }}</a
            >
            <span v-else class="font-medium">{{ user.telegram_id }}</span>
          </div>
          <div class="flex flex-col gap-1">
            <span class="text-xs text-[var(--ui-text-muted)] uppercase">{{
              t('adminUserDetail.created')
            }}</span>
            <span class="font-medium">{{ formatDate(user.created_at) }}</span>
          </div>
        </div>
      </UCard>

      <!-- Peers Section -->
      <UCard class="mb-6">
        <template #header>
          <div class="flex items-center gap-2 font-semibold">
            <UIcon name="i-lucide-link" />
            {{ t('adminUserDetail.peers', { count: user.peers.length }) }}
          </div>
        </template>
        <div
          v-if="user.peers.length"
          class="grid grid-cols-[repeat(auto-fill,minmax(280px,1fr))] gap-4"
        >
          <UCard v-for="peer in user.peers" :key="peer.id" class="bg-[var(--ui-bg-elevated)]">
            <div class="flex justify-between items-center mb-2">
              <span class="font-mono font-semibold">{{ peer.assigned_ip }}</span>
              <StatusBadge :status="peer.sync_status as PeerSyncStatus" />
            </div>
            <div
              v-if="peer.device_name"
              class="flex items-center gap-1.5 mb-2 text-sm text-[var(--ui-text-muted)]"
            >
              <UIcon name="i-lucide-monitor-smartphone" class="size-3.5" />
              <span>{{ t('adminUserDetail.device', { name: peer.device_name }) }}</span>
            </div>
            <div class="flex gap-4 mb-2">
              <div class="flex items-center gap-1">
                <UIcon name="i-lucide-arrow-up" class="text-green-500" />
                <span>{{ formatBytes(peer.tx_bytes) }}</span>
              </div>
              <div class="flex items-center gap-1">
                <UIcon name="i-lucide-arrow-down" class="text-[var(--ui-primary)]" />
                <span>{{ formatBytes(peer.rx_bytes) }}</span>
              </div>
            </div>
            <div class="text-sm text-[var(--ui-text-muted)] mb-2">
              <span v-if="peer.last_handshake">{{
                t('adminUserDetail.last', { date: formatDate(peer.last_handshake) })
              }}</span>
              <span v-else>{{ t('common.neverConnected') }}</span>
            </div>
            <div
              v-if="activeSubscription"
              class="flex flex-col gap-1 p-2 bg-[var(--ui-bg)] rounded text-sm mb-3"
            >
              <small>Speed: {{ formatLimit(activeSubscription.speed_limit_mbps) }}</small>
              <small>
                Traffic: {{ formatBytes(peer.traffic_used_bytes) }}
                <template v-if="activeSubscription.traffic_limit_bytes">
                  / {{ formatTrafficLimit(activeSubscription.traffic_limit_bytes) }}
                </template>
              </small>
            </div>
            <UButton
              v-if="peer.sync_status === 'active'"
              :label="t('common.remove')"
              icon="i-lucide-trash-2"
              color="error"
              size="sm"
              class="w-full"
              @click="confirmRemovePeer(peer.id, peer.assigned_ip)"
            />
          </UCard>
        </div>
        <div v-else class="text-center py-8 text-[var(--ui-text-muted)]">
          {{ t('adminUserDetail.noPeers') }}
        </div>
      </UCard>

      <!-- Subscription Section -->
      <UCard class="mb-6">
        <template #header>
          <div class="flex justify-between items-center">
            <div class="flex items-center gap-2 font-semibold">
              <UIcon name="i-lucide-ticket" />
              {{ t('adminUserDetail.subscription') }}
            </div>
            <div class="flex gap-2">
              <UButton
                v-if="activeSubscription"
                :label="t('adminUserDetail.changeSubscription')"
                icon="i-lucide-pencil"
                size="sm"
                @click="openSetSubDialog"
              />
              <UButton
                v-else
                :label="t('adminUserDetail.createSubscription')"
                icon="i-lucide-plus"
                size="sm"
                @click="openSetSubDialog"
              />
              <UButton
                v-if="activeSubscription"
                icon="i-lucide-trash-2"
                color="error"
                variant="outline"
                size="sm"
                @click="deleteSubConfirm = true"
              />
            </div>
          </div>
        </template>

        <!-- Active subscription card -->
        <template v-if="activeSubscription">
          <div class="p-4 rounded-lg border border-[var(--ui-border)] bg-[var(--ui-bg-elevated)]">
            <div class="flex items-center gap-3 mb-3">
              <span class="text-lg font-semibold">{{ activeSubscription.plan_display_name }}</span>
              <UBadge
                v-if="!activeSubscription.expires_at"
                color="success"
                :label="t('userDashboard.permanentLabel')"
                variant="subtle"
              />
            </div>
            <div class="grid grid-cols-2 sm:grid-cols-4 gap-4 text-sm">
              <div class="flex flex-col gap-0.5">
                <span class="text-[var(--ui-text-muted)]">{{ t('adminUserDetail.starts') }}</span>
                <span>{{ formatDate(activeSubscription.starts_at) }}</span>
              </div>
              <div class="flex flex-col gap-0.5">
                <span class="text-[var(--ui-text-muted)]">{{ t('adminUserDetail.expires') }}</span>
                <span v-if="activeSubscription.expires_at">{{
                  formatDate(activeSubscription.expires_at)
                }}</span>
                <UBadge
                  v-else
                  color="success"
                  :label="t('userDashboard.permanentLabel')"
                  variant="subtle"
                  size="sm"
                />
              </div>
              <div class="flex flex-col gap-0.5">
                <span class="text-[var(--ui-text-muted)]">{{ t('adminUserDetail.speed') }}</span>
                <span>{{ formatLimit(activeSubscription.speed_limit_mbps) }}</span>
              </div>
              <div class="flex flex-col gap-0.5">
                <span class="text-[var(--ui-text-muted)]">{{ t('adminUserDetail.traffic') }}</span>
                <span>{{ formatTrafficLimit(activeSubscription.traffic_limit_bytes) }}</span>
              </div>
            </div>
          </div>
        </template>
        <div v-else class="text-center py-8 text-[var(--ui-text-muted)]">
          {{ t('adminUserDetail.noActiveSubscription') }}
        </div>

        <!-- Subscription history -->
        <template v-if="historySubscriptions.length">
          <div class="mt-4">
            <button
              class="text-sm text-[var(--ui-text-muted)] hover:text-[var(--ui-text)] flex items-center gap-1"
              @click="showHistory = !showHistory"
            >
              <UIcon
                :name="showHistory ? 'i-lucide-chevron-down' : 'i-lucide-chevron-right'"
                class="size-4"
              />
              {{ t('adminUserDetail.subscriptionHistory') }} ({{ historySubscriptions.length }})
            </button>
            <div v-if="showHistory" class="mt-2 flex flex-col gap-2">
              <div
                v-for="sub in historySubscriptions"
                :key="sub.id"
                class="p-3 rounded border border-[var(--ui-border)] text-sm opacity-70"
              >
                <div class="flex items-center gap-2 mb-1">
                  <span class="font-medium">{{ sub.plan_display_name }}</span>
                </div>
                <div class="text-[var(--ui-text-muted)]">
                  {{ formatDate(sub.starts_at) }} &mdash;
                  {{
                    sub.expires_at ? formatDate(sub.expires_at) : t('userDashboard.permanentLabel')
                  }}
                </div>
              </div>
            </div>
          </div>
        </template>
      </UCard>
    </template>

    <!-- Set Subscription Dialog -->
    <UModal
      v-model:open="subDialog"
      :title="
        activeSubscription
          ? t('adminUserDetail.changeSubscription')
          : t('adminUserDetail.createSubscription')
      "
    >
      <template #body>
        <div class="flex flex-col gap-4">
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUserDetail.plan') }}</label>
            <USelect
              v-model="subPlanId"
              :items="planItems"
              :placeholder="t('adminUserDetail.selectPlan')"
            />
          </div>
          <div class="flex items-center gap-2">
            <UCheckbox v-model="subPermanent" />
            <label class="text-sm font-medium">{{ t('adminUserDetail.permanent') }}</label>
          </div>
          <div v-if="!subPermanent" class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUserDetail.days') }}</label>
            <UInput type="number" v-model.number="subDays" :min="1" />
          </div>
        </div>
      </template>
      <template #footer>
        <UButton
          :label="t('common.cancel')"
          color="neutral"
          variant="outline"
          @click="subDialog = false"
        />
        <UButton
          :label="t('common.save')"
          :loading="submitting"
          :disabled="!subPlanId"
          @click="setSubscription"
        />
      </template>
    </UModal>

    <!-- Delete Subscription Confirm Dialog -->
    <UModal v-model:open="deleteSubConfirm" :title="t('adminUserDetail.deleteSubscription')">
      <template #body>
        <p>{{ t('adminUserDetail.deleteSubscriptionConfirm') }}</p>
      </template>
      <template #footer>
        <UButton
          :label="t('common.cancel')"
          color="neutral"
          variant="outline"
          @click="deleteSubConfirm = false"
        />
        <UButton
          :label="t('common.delete')"
          color="error"
          :loading="deleteSubMut.asyncStatus.value === 'loading'"
          @click="deleteSubscription"
        />
      </template>
    </UModal>

    <!-- Confirm Remove Peer Dialog -->
    <UModal v-model:open="confirmOpen" :title="t('adminUserDetail.removePeer')">
      <template #body>
        <p>{{ confirmMessage }}</p>
      </template>
      <template #footer>
        <UButton
          :label="t('common.cancel')"
          color="neutral"
          variant="outline"
          @click="confirmOpen = false"
        />
        <UButton
          :label="t('common.remove')"
          color="error"
          :loading="deletePeerMut.asyncStatus.value === 'loading'"
          @click="doRemovePeer"
        />
      </template>
    </UModal>
  </div>
</template>
