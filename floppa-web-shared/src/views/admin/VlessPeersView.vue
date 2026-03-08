<script setup lang="ts">
import { ref, computed } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import {
  listVlessPeersQuery,
  regenerateAdminVlessConfigMutation,
} from '../../client/@pinia/colada.gen'
import type { VlessPeerSummary } from '../../client/types.gen'
import { formatBytes } from '../../utils'
import type { TableColumn } from '@nuxt/ui'

const router = useRouter()
const { t } = useI18n()
const toast = useToast()
const { data: peers, status, error, refresh: refreshPeers } = useQuery(listVlessPeersQuery())
const regenerateMut = useMutation(regenerateAdminVlessConfigMutation())
const search = ref('')

const confirmOpen = ref(false)
const confirmUsername = ref('')
const pendingUserId = ref<number | null>(null)

const filteredPeers = computed(() => {
  if (!peers.value) return []
  if (!search.value) return peers.value
  const q = search.value.toLowerCase()
  return peers.value.filter(
    (p) =>
      p.username?.toLowerCase().includes(q) ||
      p.device_name?.toLowerCase().includes(q) ||
      p.plan_name?.toLowerCase().includes(q),
  )
})

function confirmRegenerate(userId: number, username: string | null | undefined) {
  pendingUserId.value = userId
  confirmUsername.value = username || '-'
  confirmOpen.value = true
}

async function doRegenerate() {
  if (!pendingUserId.value) return
  try {
    await regenerateMut.mutateAsync({ path: { id: pendingUserId.value } })
    await refreshPeers()
    toast.add({
      title: t('common.success'),
      description: t('adminVless.regenerated'),
      color: 'success',
    })
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: e instanceof Error ? e.message : t('adminVless.regenerateFailed'),
      color: 'error',
    })
  }
  confirmOpen.value = false
  pendingUserId.value = null
}

const columns = computed<TableColumn<VlessPeerSummary>[]>(() => [
  { accessorKey: 'username', header: t('adminVless.user') },
  { accessorKey: 'device_name', header: t('adminVless.device') },
  { accessorKey: 'app_version', header: t('adminVless.version') },
  { accessorKey: 'plan_name', header: t('adminVless.plan') },
  { accessorKey: 'download_bytes', header: t('adminVless.download') },
  { accessorKey: 'upload_bytes', header: t('adminVless.upload') },
  { accessorKey: 'has_wg', header: 'WG' },
  { id: 'actions', header: '' },
])
</script>

<template>
  <div class="max-w-7xl mx-auto">
    <div class="flex justify-between items-center mb-6 flex-wrap gap-4">
      <h1 class="text-2xl font-bold">{{ t('adminVless.title') }}</h1>
      <UInput
        v-model="search"
        :placeholder="t('adminVless.searchPlaceholder')"
        icon="i-lucide-search"
        class="w-full sm:w-64"
      />
    </div>

    <div v-if="status === 'pending'" class="flex justify-center py-12">
      <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
    </div>
    <UAlert v-else-if="error" color="error" :title="error.message" />
    <template v-else>
      <!-- Desktop table -->
      <div class="hidden md:block">
        <UTable
          :data="filteredPeers"
          :columns="columns"
          class="[&_tbody_tr]:cursor-pointer"
          @select="(_e: Event, row: any) => router.push(`/admin/users/${row.original.user_id}`)"
        >
          <template #username-cell="{ row }">
            {{ row.original.username || '-' }}
          </template>
          <template #device_name-cell="{ row }">
            <span
              v-if="row.original.device_name"
              class="flex items-center gap-1.5 max-w-40"
              :title="row.original.device_name"
            >
              <UIcon
                name="i-lucide-monitor-smartphone"
                class="size-4 shrink-0 text-[var(--ui-text-muted)]"
              />
              <span class="truncate">{{ row.original.device_name }}</span>
            </span>
            <span v-else class="text-[var(--ui-text-muted)]">-</span>
          </template>
          <template #app_version-cell="{ row }">
            <span v-if="row.original.app_version" class="font-mono text-xs">{{
              row.original.app_version
            }}</span>
            <span v-else class="text-[var(--ui-text-muted)]">-</span>
          </template>
          <template #plan_name-cell="{ row }">
            {{ row.original.plan_name || '-' }}
          </template>
          <template #download_bytes-cell="{ row }">
            {{ formatBytes(row.original.download_bytes) }}
          </template>
          <template #upload_bytes-cell="{ row }">
            {{ formatBytes(row.original.upload_bytes) }}
          </template>
          <template #has_wg-cell="{ row }">
            <UIcon
              :name="row.original.has_wg ? 'i-lucide-check' : 'i-lucide-x'"
              :class="row.original.has_wg ? 'text-green-500' : 'text-[var(--ui-text-muted)]'"
              class="size-4"
            />
          </template>
          <template #actions-cell="{ row }">
            <UButton
              icon="i-lucide-refresh-cw"
              color="warning"
              variant="ghost"
              size="xs"
              @click.stop="confirmRegenerate(row.original.user_id, row.original.username)"
            />
          </template>
          <template #empty>
            <div class="text-center py-8 text-[var(--ui-text-muted)]">
              {{ t('adminVless.noConfigs') }}
            </div>
          </template>
        </UTable>
      </div>

      <!-- Mobile cards -->
      <div class="md:hidden flex flex-col gap-3">
        <div v-if="filteredPeers.length === 0" class="text-center py-8 text-[var(--ui-text-muted)]">
          {{ t('adminVless.noConfigs') }}
        </div>
        <UCard
          v-for="peer in filteredPeers"
          :key="peer.user_id"
          class="cursor-pointer active:scale-[0.98] transition-transform"
          @click="router.push(`/admin/users/${peer.user_id}`)"
        >
          <div class="flex justify-between items-start">
            <div>
              <span class="font-medium">{{ peer.username || '-' }}</span>
              <span
                v-if="peer.device_name"
                class="flex items-center gap-1.5 text-sm text-[var(--ui-text-muted)] mt-0.5"
              >
                <UIcon name="i-lucide-monitor-smartphone" class="size-4" />
                {{ peer.device_name }}
              </span>
            </div>
            <UButton
              icon="i-lucide-refresh-cw"
              color="warning"
              variant="ghost"
              size="xs"
              @click.stop="confirmRegenerate(peer.user_id, peer.username)"
            />
          </div>
          <div class="flex gap-3 mt-2 text-xs text-[var(--ui-text-muted)] flex-wrap items-center">
            <span v-if="peer.plan_name">{{ peer.plan_name }}</span>
            <span v-if="peer.app_version" class="font-mono">v{{ peer.app_version }}</span>
            <span>↓ {{ formatBytes(peer.download_bytes) }}</span>
            <span>↑ {{ formatBytes(peer.upload_bytes) }}</span>
            <span class="flex items-center gap-1">
              WG
              <UIcon
                :name="peer.has_wg ? 'i-lucide-check' : 'i-lucide-x'"
                :class="peer.has_wg ? 'text-green-500' : ''"
                class="size-3.5"
              />
            </span>
          </div>
        </UCard>
      </div>
    </template>

    <!-- Confirm Regenerate Dialog -->
    <UModal v-model:open="confirmOpen" :title="t('adminVless.regenerateTitle')">
      <template #body>
        <p>{{ t('adminVless.regenerateConfirm', { user: confirmUsername }) }}</p>
      </template>
      <template #footer>
        <UButton
          :label="t('common.cancel')"
          color="neutral"
          variant="outline"
          @click="confirmOpen = false"
        />
        <UButton
          :label="t('adminVless.regenerate')"
          color="warning"
          :loading="regenerateMut.asyncStatus.value === 'loading'"
          @click="doRegenerate"
        />
      </template>
    </UModal>
  </div>
</template>
