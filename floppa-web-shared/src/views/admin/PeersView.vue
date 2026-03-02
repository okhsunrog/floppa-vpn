<script setup lang="ts">
import { ref, computed } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import { listPeersQuery, deleteAdminPeerMutation } from '../../client/@pinia/colada.gen'
import type { PeerSummary } from '../../client/types.gen'
import { formatBytes } from '../../utils'
import StatusBadge from '../../components/StatusBadge.vue'
import type { PeerSyncStatus } from '../../types'
import type { TableColumn } from '@nuxt/ui'

const router = useRouter()
const { t } = useI18n()
const toast = useToast()
const { data: peers, status, error, refresh: refreshPeers } = useQuery(listPeersQuery())
const deleteMut = useMutation(deleteAdminPeerMutation())
const search = ref('')

const confirmOpen = ref(false)
const confirmMessage = ref('')
const pendingPeerId = ref<number | null>(null)

const filteredPeers = computed(() => {
  if (!peers.value) return []
  if (!search.value) return peers.value
  const q = search.value.toLowerCase()
  return peers.value.filter(p =>
    p.assigned_ip.includes(q) ||
    p.username?.toLowerCase().includes(q) ||
    p.device_name?.toLowerCase().includes(q) ||
    p.device_id?.toLowerCase().includes(q)
  )
})

function formatDate(date: string): string {
  return new Date(date).toLocaleString()
}

function confirmDeletePeer(peerId: number, peerIp: string) {
  pendingPeerId.value = peerId
  confirmMessage.value = t('adminPeers.deleteConfirm', { ip: peerIp })
  confirmOpen.value = true
}

async function doDeletePeer() {
  if (!pendingPeerId.value) return
  try {
    await deleteMut.mutateAsync({ path: { id: pendingPeerId.value } })
    await refreshPeers()
    toast.add({ title: t('common.success'), description: t('adminPeers.peerDeleted'), color: 'success' })
  } catch (e) {
    toast.add({ title: t('common.error'), description: e instanceof Error ? e.message : t('adminPeers.deleteFailed'), color: 'error' })
  }
  confirmOpen.value = false
  pendingPeerId.value = null
}

const columns = computed<TableColumn<PeerSummary>[]>(() => [
  { accessorKey: 'assigned_ip', header: t('adminPeers.ip') },
  { accessorKey: 'username', header: t('adminPeers.user') },
  { accessorKey: 'device_name', header: t('adminPeers.device') },
  { accessorKey: 'sync_status', header: t('adminPeers.status') },
  { accessorKey: 'tx_bytes', header: 'TX' },
  { accessorKey: 'rx_bytes', header: 'RX' },
  { accessorKey: 'last_handshake', header: t('adminPeers.lastSeen') },
  { id: 'actions', header: '' },
])
</script>

<template>
  <div class="max-w-6xl mx-auto">
    <div class="flex justify-between items-center mb-6 flex-wrap gap-4">
      <h1 class="text-2xl font-bold">{{ t('adminPeers.title') }}</h1>
      <UInput v-model="search" :placeholder="t('adminPeers.searchPlaceholder')" icon="i-lucide-search" class="w-full sm:w-64" />
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
        <template #assigned_ip-cell="{ row }">
          <span class="font-mono font-medium">{{ row.original.assigned_ip }}</span>
        </template>
        <template #username-cell="{ row }">
          {{ row.original.username || '-' }}
        </template>
        <template #device_name-cell="{ row }">
          <span v-if="row.original.device_name" class="flex items-center gap-1.5">
            <UIcon name="i-lucide-monitor-smartphone" class="size-4 text-[var(--ui-text-muted)]" />
            {{ row.original.device_name }}
          </span>
          <span v-else class="text-[var(--ui-text-muted)]">-</span>
        </template>
        <template #sync_status-cell="{ row }">
          <StatusBadge :status="row.original.sync_status as PeerSyncStatus" />
        </template>
        <template #tx_bytes-cell="{ row }">
          {{ formatBytes(row.original.tx_bytes) }}
        </template>
        <template #rx_bytes-cell="{ row }">
          {{ formatBytes(row.original.rx_bytes) }}
        </template>
        <template #last_handshake-cell="{ row }">
          <span v-if="row.original.last_handshake">{{ formatDate(row.original.last_handshake) }}</span>
          <span v-else class="text-[var(--ui-text-muted)]">{{ t('common.neverConnected') }}</span>
        </template>
        <template #actions-cell="{ row }">
          <UButton
            v-if="row.original.sync_status === 'active'"
            icon="i-lucide-trash-2"
            color="error"
            variant="ghost"
            size="xs"
            @click.stop="confirmDeletePeer(row.original.id, row.original.assigned_ip)"
          />
        </template>
        <template #empty>
          <div class="text-center py-8 text-[var(--ui-text-muted)]">{{ t('adminPeers.noPeers') }}</div>
        </template>
      </UTable>
    </div>

    <!-- Mobile cards -->
    <div class="md:hidden flex flex-col gap-3">
      <div v-if="filteredPeers.length === 0" class="text-center py-8 text-[var(--ui-text-muted)]">
        {{ t('adminPeers.noPeers') }}
      </div>
      <UCard
        v-for="peer in filteredPeers"
        :key="peer.id"
        class="cursor-pointer active:scale-[0.98] transition-transform"
        @click="router.push(`/admin/users/${peer.user_id}`)"
      >
        <div class="flex justify-between items-start">
          <div>
            <span class="font-mono font-medium">{{ peer.assigned_ip }}</span>
            <span class="block text-sm">{{ peer.username || '-' }}</span>
          </div>
          <div class="flex items-center gap-2">
            <StatusBadge :status="peer.sync_status as PeerSyncStatus" />
            <UButton
              v-if="peer.sync_status === 'active'"
              icon="i-lucide-trash-2"
              color="error"
              variant="ghost"
              size="xs"
              @click.stop="confirmDeletePeer(peer.id, peer.assigned_ip)"
            />
          </div>
        </div>
        <div v-if="peer.device_name" class="flex items-center gap-1.5 mt-1.5 text-sm text-[var(--ui-text-muted)]">
          <UIcon name="i-lucide-monitor-smartphone" class="size-4" />
          {{ peer.device_name }}
        </div>
        <div class="flex gap-4 mt-2 text-xs text-[var(--ui-text-muted)]">
          <span>TX: {{ formatBytes(peer.tx_bytes) }}</span>
          <span>RX: {{ formatBytes(peer.rx_bytes) }}</span>
          <span v-if="peer.last_handshake">{{ formatDate(peer.last_handshake) }}</span>
          <span v-else>{{ t('common.neverConnected') }}</span>
        </div>
      </UCard>
    </div>
    </template>

    <!-- Confirm Delete Dialog -->
    <UModal v-model:open="confirmOpen" :title="t('adminPeers.deletePeer')">
      <template #body>
        <p>{{ confirmMessage }}</p>
      </template>
      <template #footer>
        <UButton :label="t('common.cancel')" color="neutral" variant="outline" @click="confirmOpen = false" />
        <UButton :label="t('common.delete')" color="error" :loading="deleteMut.asyncStatus.value === 'loading'" @click="doDeletePeer" />
      </template>
    </UModal>
  </div>
</template>
