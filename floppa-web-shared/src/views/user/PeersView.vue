<script setup lang="ts">
import { ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import { getMeQuery, getMyPeersQuery, createMyPeerMutation, deleteMyPeerMutation } from '../../client/@pinia/colada.gen'
import { getMyPeerConfig, sendMyPeerConfig } from '../../client/sdk.gen'
import type { CreatePeerResponse, MyPeer } from '../../client/types.gen'
import { formatBytes } from '../../utils'
import StatusBadge from '../../components/StatusBadge.vue'
import type { PeerSyncStatus } from '../../types'

const { t } = useI18n()
const toast = useToast()

const isMiniApp = Boolean((window as { Telegram?: { WebApp?: { initData?: string } } }).Telegram?.WebApp?.initData)

const { data: me, status: meStatus, error: meError } = useQuery(getMeQuery())
const { data: peers, status: peersStatus, error: peersError, refresh: refreshPeers } = useQuery(getMyPeersQuery())

const loading = computed(() => meStatus.value === 'pending' || peersStatus.value === 'pending')
const queryError = computed(() => meError.value || peersError.value)
const queryErrorMessage = computed(() => {
  const err = queryError.value
  if (!err) return ''
  if (err instanceof TypeError) return t('common.serverUnavailable')
  return err.message
})

const createMut = useMutation(createMyPeerMutation())
const deleteMut = useMutation(deleteMyPeerMutation())

const configDialog = ref(false)
const currentConfig = ref<CreatePeerResponse | null>(null)

// Confirm delete state
const confirmOpen = ref(false)
const confirmMessage = ref('')
const pendingDeletePeerId = ref<number | null>(null)

const canCreatePeer = computed(() => {
  if (!me.value?.subscription) return false
  if (!peers.value) return false
  const activePeers = peers.value.filter(p => p.sync_status !== 'pending_remove').length
  return activePeers < me.value.subscription.max_peers
})

const peersRemaining = computed(() => {
  if (!me.value?.subscription) return 0
  if (!peers.value) return 0
  const activePeers = peers.value.filter(p => p.sync_status !== 'pending_remove').length
  return me.value.subscription.max_peers - activePeers
})

async function createPeer() {
  if (!canCreatePeer.value) return
  try {
    const response = await createMut.mutateAsync({})
    currentConfig.value = response
    configDialog.value = true
    await refreshPeers()
    toast.add({ title: t('userPeers.configCreated'), description: t('userPeers.configCreatedMessage'), color: 'success' })
  } catch (e) {
    toast.add({ title: t('common.error'), description: e instanceof Error ? e.message : t('userPeers.createFailed'), color: 'error' })
  }
}

function confirmDeletePeer(peerId: number, peerIp: string) {
  pendingDeletePeerId.value = peerId
  confirmMessage.value = t('userPeers.deleteConfirm', { ip: peerIp })
  confirmOpen.value = true
}

async function doDeletePeer() {
  if (!pendingDeletePeerId.value) return
  try {
    await deleteMut.mutateAsync({ path: { id: pendingDeletePeerId.value } })
    await refreshPeers()
    toast.add({ title: t('userPeers.configDeleted'), description: t('userPeers.configDeletedMessage'), color: 'success' })
  } catch (e) {
    toast.add({ title: t('common.error'), description: e instanceof Error ? e.message : t('userPeers.deleteFailed'), color: 'error' })
  }
  confirmOpen.value = false
  pendingDeletePeerId.value = null
}

async function showConfig(peer: MyPeer) {
  try {
    const { data } = await getMyPeerConfig({ path: { id: peer.id }, throwOnError: true })
    currentConfig.value = {
      id: peer.id,
      assigned_ip: peer.assigned_ip,
      config: data,
    }
    configDialog.value = true
  } catch (e) {
    toast.add({ title: t('common.error'), description: e instanceof Error ? e.message : t('userPeers.createFailed'), color: 'error' })
  }
}

async function copyConfig() {
  if (currentConfig.value) {
    await navigator.clipboard.writeText(currentConfig.value.config)
    toast.add({ title: t('common.copied'), description: t('userPeers.copiedMessage'), color: 'success' })
  }
}

async function downloadConfig() {
  if (!currentConfig.value) return
  const { id, assigned_ip, config } = currentConfig.value
  const filename = `floppa-vpn-${assigned_ip}.conf`

  console.info('[download] downloadConfig called, isMiniApp:', isMiniApp, '__TAURI_INTERNALS__:', Boolean((window as unknown as Record<string, unknown>).__TAURI_INTERNALS__), 'userAgent:', navigator.userAgent)

  if (isMiniApp) {
    try {
      await sendMyPeerConfig({ path: { id }, throwOnError: true })
      toast.add({ title: t('userPeers.sentToTelegram'), color: 'success' })
    } catch {
      toast.add({ title: t('userPeers.sendFailed'), color: 'error' })
    }
    return
  }

  // Tauri: platform-specific file saving
  if ((window as unknown as Record<string, unknown>).__TAURI_INTERNALS__) {
    try {
      if (navigator.userAgent.includes('Android')) {
        // Android: save to Download/FloppaVPN/ via MediaStore
        console.info('[download] Android detected, using android-fs plugin')
        const { AndroidFs, AndroidPublicGeneralPurposeDir } = await import('tauri-plugin-android-fs-api')
        console.info('[download] Creating public file...')
        const uri = await AndroidFs.createNewPublicFile(
          AndroidPublicGeneralPurposeDir.Download,
          `FloppaVPN/${filename}`,
          'text/plain',
        )
        console.info('[download] File created, uri:', JSON.stringify(uri))
        await AndroidFs.writeTextFile(uri, config)
        console.info('[download] File written successfully')
        toast.add({ title: t('userPeers.configSaved', { path: `Download/FloppaVPN/${filename}` }), color: 'success' })
      } else {
        // Desktop: save dialog + write file
        const { save } = await import('@tauri-apps/plugin-dialog')
        const { writeTextFile } = await import('@tauri-apps/plugin-fs')
        const path = await save({ defaultPath: filename, filters: [{ name: 'WireGuard', extensions: ['conf'] }] })
        if (path) await writeTextFile(path, config)
      }
    } catch (e) {
      console.error('[download] Failed:', e)
      toast.add({ title: t('common.error'), color: 'error' })
    }
    return
  }

  // Browser fallback
  const blob = new Blob([config], { type: 'text/plain' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  a.click()
  URL.revokeObjectURL(url)
}

function formatDate(date: string): string {
  return new Date(date).toLocaleString()
}

function formatShortDate(date: string): string {
  return new Date(date).toLocaleDateString()
}

function formatTrafficLimit(bytes: number | null | undefined): string {
  if (bytes == null) return t('common.unlimited')
  const gb = bytes / (1024 * 1024 * 1024)
  return `${gb.toFixed(1)} GB`
}
</script>

<template>
  <div class="max-w-4xl mx-auto">
    <div class="flex justify-between items-start mb-6 flex-wrap gap-4">
      <div>
        <h1 class="text-2xl font-bold">{{ t('userPeers.title') }}</h1>
        <p v-if="me?.subscription" class="text-sm text-[var(--ui-text-muted)] mt-1">
          {{ t('userPeers.remaining', { count: peersRemaining, max: me.subscription.max_peers }) }}
        </p>
      </div>
      <UButton
        v-if="me?.subscription"
        :label="t('userPeers.createNew')"
        icon="i-lucide-plus"
        :disabled="!canCreatePeer"
        :loading="createMut.asyncStatus.value === 'loading'"
        @click="createPeer"
      />
    </div>

    <div v-if="loading" class="flex justify-center py-12">
      <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
    </div>
    <UAlert v-else-if="queryError" color="error" :title="queryErrorMessage" />
    <template v-else>
      <!-- No subscription message -->
      <UCard v-if="!me?.subscription" class="max-w-sm mx-auto">
        <div class="flex flex-col items-center text-center py-8">
          <UIcon name="i-lucide-lock" class="text-4xl text-[var(--ui-text-muted)] mb-4" />
          <p>{{ t('userPeers.noSubscription') }}</p>
          <p class="text-sm text-[var(--ui-text-muted)] mt-1">{{ t('userPeers.contactSupport') }}</p>
        </div>
      </UCard>

      <!-- Empty state -->
      <UCard v-else-if="!peers?.length" class="max-w-sm mx-auto">
        <div class="flex flex-col items-center text-center py-8">
          <UIcon name="i-lucide-key" class="text-4xl text-[var(--ui-text-muted)] mb-4" />
          <p>{{ t('userPeers.noConfigs') }}</p>
          <p class="text-sm text-[var(--ui-text-muted)] mt-1">{{ t('userPeers.noConfigsHint') }}</p>
        </div>
      </UCard>

      <!-- Peers grid -->
      <div v-else class="grid grid-cols-[repeat(auto-fill,minmax(300px,1fr))] gap-4">
        <UCard v-for="peer in peers" :key="peer.id">
          <div class="flex justify-between items-center mb-2">
            <span class="font-mono text-lg font-semibold">{{ peer.assigned_ip }}</span>
            <StatusBadge :status="peer.sync_status as PeerSyncStatus" />
          </div>
          <div v-if="peer.device_name" class="flex items-center gap-1.5 mb-3 text-sm text-[var(--ui-text-muted)]">
            <UIcon name="i-lucide-monitor-smartphone" class="size-4" />
            <span>{{ t('userPeers.device', { name: peer.device_name }) }}</span>
          </div>
          <div class="flex gap-6 mb-3">
            <div class="flex items-center gap-2">
              <UIcon name="i-lucide-arrow-up" class="text-green-500" />
              <div>
                <span class="font-medium">{{ formatBytes(peer.tx_bytes) }}</span>
                <span class="ml-1 text-xs text-[var(--ui-text-muted)]">TX</span>
              </div>
            </div>
            <div class="flex items-center gap-2">
              <UIcon name="i-lucide-arrow-down" class="text-[var(--ui-primary)]" />
              <div>
                <span class="font-medium">{{ formatBytes(peer.rx_bytes) }}</span>
                <span class="ml-1 text-xs text-[var(--ui-text-muted)]">RX</span>
              </div>
            </div>
          </div>
          <div v-if="me?.subscription?.traffic_limit_bytes" class="flex items-center gap-1.5 mb-3 text-sm text-[var(--ui-text-muted)]">
            <UIcon name="i-lucide-gauge" class="size-4" />
            <span>{{ t('userPeers.trafficUsed', { used: formatBytes(peer.traffic_used_bytes), limit: formatTrafficLimit(me.subscription.traffic_limit_bytes) }) }}</span>
          </div>
          <div class="flex flex-col gap-1 text-sm text-[var(--ui-text-muted)] mb-4">
            <span v-if="peer.last_handshake">{{ t('userPeers.lastSeen', { date: formatDate(peer.last_handshake) }) }}</span>
            <span v-else>{{ t('common.neverConnected') }}</span>
            <span>{{ t('userPeers.createdAt', { date: formatShortDate(peer.created_at) }) }}</span>
          </div>
          <div class="flex gap-2">
            <UButton
              v-if="!peer.device_name"
              :label="t('userPeers.showConfig')"
              icon="i-lucide-eye"
              size="sm"
              @click="showConfig(peer)"
            />
            <UButton
              v-if="peer.sync_status === 'active'"
              icon="i-lucide-trash-2"
              color="error"
              variant="ghost"
              size="sm"
              @click="confirmDeletePeer(peer.id, peer.assigned_ip)"
            />
          </div>
        </UCard>
      </div>
    </template>

    <!-- Config Dialog -->
    <UModal v-model:open="configDialog" :title="t('userPeers.configTitle')">
      <template #body>
        <div v-if="currentConfig" class="flex flex-col gap-4">
          <p class="flex items-center gap-2 text-[var(--ui-text-muted)]">
            <UIcon name="i-lucide-globe" />
            {{ t('userPeers.ip', { ip: currentConfig.assigned_ip }) }}
          </p>
          <pre class="bg-[var(--ui-bg-inverted)] text-[var(--ui-text-inverted)] p-4 rounded-lg overflow-x-auto text-sm whitespace-pre-wrap break-all">{{ currentConfig.config }}</pre>
        </div>
      </template>
      <template #footer>
        <UButton :label="t('common.copy')" icon="i-lucide-copy" @click="copyConfig" />
        <UButton
          :label="isMiniApp ? t('userPeers.sendToTelegram') : t('common.download')"
          :icon="isMiniApp ? 'i-lucide-send' : 'i-lucide-download'"
          color="success"
          @click="downloadConfig"
        />
        <UButton :label="t('common.close')" color="neutral" variant="outline" @click="configDialog = false" />
      </template>
    </UModal>

    <!-- Confirm Delete Dialog -->
    <UModal v-model:open="confirmOpen" :title="t('userPeers.deleteConfig')">
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
