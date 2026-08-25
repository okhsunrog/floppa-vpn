<script setup lang="ts">
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import {
  listPeersQuery,
  listPeersQueryKey,
  deleteAdminPeerMutation,
} from '../../client/@pinia/colada.gen'
import type { PeerSummary } from '../../client/types.gen'
import { describeError, formatBytes, formatDateTime } from '../../utils'
import type { TableColumn } from '@nuxt/ui'
import { useAdminList, useConfirmAction } from '../../composables/adminList'
import { useInvalidateQueries } from '../../composables/invalidate'
import AdminListPage from '../../components/AdminListPage.vue'
import ConfirmModal from '../../components/ConfirmModal.vue'

const router = useRouter()
const { t } = useI18n()
const toast = useToast()
const { data: peers, status, error } = useQuery(listPeersQuery())
const invalidate = useInvalidateQueries()
const deleteMut = useMutation({
  ...deleteAdminPeerMutation(),
  onSettled: () => invalidate(listPeersQueryKey()),
})

const {
  open: confirmOpen,
  message: confirmMessage,
  request: requestDeletePeer,
  confirm: runDeletePeer,
} = useConfirmAction()

const {
  search,
  filtered: filteredPeers,
  page,
  paginated: paginatedPeers,
  pageSize,
} = useAdminList(peers, (p) => [p.assigned_ip, p.username, p.device_name, p.device_id])

function confirmDeletePeer(peerId: number, peerIp: string) {
  requestDeletePeer(peerId, t('adminPeers.deleteConfirm', { ip: peerIp }))
}

async function doDeletePeer() {
  await runDeletePeer(async (id) => {
    try {
      await deleteMut.mutateAsync({ path: { id } })
      toast.add({
        title: t('common.success'),
        description: t('adminPeers.peerDeleted'),
        color: 'success',
      })
    } catch (e) {
      toast.add({
        title: t('common.error'),
        description: describeError(e, t('adminPeers.deleteFailed'), t),
        color: 'error',
      })
    }
  })
}

function openUser(peer: PeerSummary) {
  void router.push(`/admin/users/${peer.user_id}`)
}

const columns = computed<TableColumn<PeerSummary>[]>(() => [
  { accessorKey: 'assigned_ip', header: t('adminPeers.ip') },
  { accessorKey: 'protocol', header: t('adminPeers.protocol') },
  { accessorKey: 'username', header: t('adminPeers.user') },
  { accessorKey: 'device_name', header: t('adminPeers.device') },
  { accessorKey: 'client_version', header: t('adminPeers.version') },
  { accessorKey: 'plan_name', header: t('adminPeers.plan') },
  { accessorKey: 'download_bytes', header: t('adminPeers.download') },
  { accessorKey: 'upload_bytes', header: t('adminPeers.upload') },
  { accessorKey: 'last_handshake', header: t('adminPeers.lastSeen') },
  { accessorKey: 'has_vless', header: 'VLESS' },
  { id: 'actions', header: '' },
])
</script>

<template>
  <AdminListPage
    v-model:search="search"
    v-model:page="page"
    :title="t('adminPeers.title')"
    :status="status"
    :error="error"
    :columns="columns"
    :rows="paginatedPeers"
    :total="filteredPeers.length"
    :page-size="pageSize"
    :row-key="(peer) => peer.id"
    :search-placeholder="t('adminPeers.searchPlaceholder')"
    :empty-text="t('adminPeers.noPeers')"
    @select="openUser"
  >
    <template #assigned_ip-cell="{ row }">
      <span class="font-mono font-medium">{{ row.original.assigned_ip }}</span>
    </template>
    <template #protocol-cell="{ row }">
      <UBadge color="neutral" variant="subtle" size="sm">
        {{ t(`vpn.${row.original.protocol}`) }}
      </UBadge>
    </template>
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
    <template #client_version-cell="{ row }">
      <span v-if="row.original.client_version" class="font-mono text-xs">{{
        row.original.client_version
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
    <template #last_handshake-cell="{ row }">
      <span v-if="row.original.last_handshake">{{
        formatDateTime(row.original.last_handshake)
      }}</span>
      <span v-else class="text-[var(--ui-text-muted)]">{{ t('common.neverConnected') }}</span>
    </template>
    <template #has_vless-cell="{ row }">
      <UIcon
        :name="row.original.has_vless ? 'i-lucide-check' : 'i-lucide-x'"
        :class="row.original.has_vless ? 'text-green-500' : 'text-[var(--ui-text-muted)]'"
        class="size-4"
      />
    </template>
    <template #actions-cell="{ row }">
      <UButton
        icon="i-lucide-trash-2"
        color="error"
        variant="ghost"
        size="xs"
        @click.stop="confirmDeletePeer(row.original.id, row.original.assigned_ip)"
      />
    </template>

    <template #card="{ row: peer }">
      <div class="flex justify-between items-start">
        <div>
          <span class="font-mono font-medium">{{ peer.assigned_ip }}</span>
          <span class="block text-sm">{{ peer.username || '-' }}</span>
        </div>
        <UButton
          icon="i-lucide-trash-2"
          color="error"
          variant="ghost"
          size="xs"
          @click.stop="confirmDeletePeer(peer.id, peer.assigned_ip)"
        />
      </div>
      <div
        v-if="peer.device_name || peer.client_version"
        class="flex items-center gap-3 mt-1.5 text-sm text-[var(--ui-text-muted)]"
      >
        <span v-if="peer.device_name" class="flex items-center gap-1.5">
          <UIcon name="i-lucide-monitor-smartphone" class="size-4" />
          {{ peer.device_name }}
        </span>
        <span v-if="peer.client_version" class="font-mono text-xs">v{{ peer.client_version }}</span>
      </div>
      <div class="flex gap-3 mt-2 text-xs text-[var(--ui-text-muted)] flex-wrap items-center">
        <span v-if="peer.plan_name">{{ peer.plan_name }}</span>
        <span>↓ {{ formatBytes(peer.download_bytes) }}</span>
        <span>↑ {{ formatBytes(peer.upload_bytes) }}</span>
        <span v-if="peer.last_handshake">{{ formatDateTime(peer.last_handshake) }}</span>
        <span v-else>{{ t('common.neverConnected') }}</span>
        <span class="flex items-center gap-1">
          VLESS
          <UIcon
            :name="peer.has_vless ? 'i-lucide-check' : 'i-lucide-x'"
            :class="peer.has_vless ? 'text-green-500' : ''"
            class="size-3.5"
          />
        </span>
      </div>
    </template>

    <ConfirmModal
      v-model:open="confirmOpen"
      :title="t('adminPeers.deletePeer')"
      :message="confirmMessage"
      :confirm-label="t('common.delete')"
      confirm-color="error"
      :loading="deleteMut.asyncStatus.value === 'loading'"
      @confirm="doDeletePeer"
    />
  </AdminListPage>
</template>
