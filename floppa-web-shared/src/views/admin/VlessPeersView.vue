<script setup lang="ts">
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import {
  listVlessPeersQuery,
  listVlessPeersQueryKey,
  regenerateAdminVlessConfigMutation,
} from '../../client/@pinia/colada.gen'
import type { VlessPeerSummary } from '../../client/types.gen'
import { describeError, formatBytes } from '../../utils'
import type { TableColumn } from '@nuxt/ui'
import { useAdminList, useConfirmAction } from '../../composables/adminList'
import { useInvalidateQueries } from '../../composables/invalidate'
import AdminListPage from '../../components/AdminListPage.vue'
import ConfirmModal from '../../components/ConfirmModal.vue'

const router = useRouter()
const { t } = useI18n()
const toast = useToast()
const { data: peers, status, error } = useQuery(listVlessPeersQuery())
const invalidate = useInvalidateQueries()
const regenerateMut = useMutation({
  ...regenerateAdminVlessConfigMutation(),
  onSettled: () => invalidate(listVlessPeersQueryKey()),
})

const {
  open: confirmOpen,
  message: confirmUsername,
  request: requestRegenerate,
  confirm: runRegenerate,
} = useConfirmAction()

const {
  search,
  filtered: filteredPeers,
  page,
  paginated: paginatedPeers,
  pageSize,
} = useAdminList(peers, (p) => [p.username, p.device_name, p.plan_name])

function confirmRegenerate(userId: number, username: string | null | undefined) {
  requestRegenerate(userId, username || '-')
}

async function doRegenerate() {
  await runRegenerate(async (id) => {
    try {
      await regenerateMut.mutateAsync({ path: { id } })
      toast.add({
        title: t('common.success'),
        description: t('adminVless.regenerated'),
        color: 'success',
      })
    } catch (e) {
      toast.add({
        title: t('common.error'),
        description: describeError(e, t('adminVless.regenerateFailed'), t),
        color: 'error',
      })
    }
  })
}

function openUser(peer: VlessPeerSummary) {
  void router.push(`/admin/users/${peer.user_id}`)
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
  <AdminListPage
    v-model:search="search"
    v-model:page="page"
    :title="t('adminVless.title')"
    :status="status"
    :error="error"
    :columns="columns"
    :rows="paginatedPeers"
    :total="filteredPeers.length"
    :page-size="pageSize"
    :row-key="(peer) => peer.user_id"
    :search-placeholder="t('adminVless.searchPlaceholder')"
    :empty-text="t('adminVless.noConfigs')"
    @select="openUser"
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

    <template #card="{ row: peer }">
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
    </template>

    <ConfirmModal
      v-model:open="confirmOpen"
      :title="t('adminVless.regenerateTitle')"
      :message="t('adminVless.regenerateConfirm', { user: confirmUsername })"
      :confirm-label="t('adminVless.regenerate')"
      confirm-color="warning"
      :loading="regenerateMut.asyncStatus.value === 'loading'"
      @confirm="doRegenerate"
    />
  </AdminListPage>
</template>
