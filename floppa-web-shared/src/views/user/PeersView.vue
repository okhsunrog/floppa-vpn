<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import {
  getMeQuery,
  getMyPeersQuery,
  getMyPeersQueryKey,
  getMyRegionsQuery,
  getPublicConfigQuery,
  createMyPeerMutation,
  deleteMyPeerMutation,
  regenerateMyVlessConfigMutation,
} from '../../client/@pinia/colada.gen'
import { getMyPeerConfig, getMyVlessConfig, sendMyPeerConfig } from '../../client/sdk.gen'
import type { CreatePeerResponse, MyPeer } from '../../client/types.gen'
import { describeError, formatTraffic, formatDate, formatDateTime } from '../../utils'
import { isTauri } from '../../utils/platform'
import { isMiniApp as detectMiniApp } from '../../utils/telegram'
import StatusBadge from '../../components/StatusBadge.vue'
import { useInvalidateQueries } from '../../composables/invalidate'
import { useConfirmAction } from '../../composables/adminList'

const { t } = useI18n()
const toast = useToast()

const isMiniApp = detectMiniApp()

const { data: me, status: meStatus, error: meError } = useQuery(getMeQuery())
const { data: publicConfig } = useQuery(getPublicConfigQuery())
const { data: peersData, status: peersStatus, error: peersError } = useQuery(getMyPeersQuery())
const {
  data: regions,
  status: regionsStatus,
  error: regionsError,
  refresh: refreshRegions,
} = useQuery({
  ...getMyRegionsQuery(),
  enabled: computed(() => !!me.value?.subscription),
})
const configRegion = ref('europe')
const regionItems = computed(() =>
  (regions.value ?? [])
    .filter((region) => region.available)
    .map((region) => ({
      label: t(`vpn.regions.${region.id}`, region.display_name),
      value: region.id,
    })),
)
watch(regionItems, (items) => {
  if (items.length && !items.some((item) => item.value === configRegion.value)) {
    configRegion.value = items[0]!.value
  }
})

const amneziaWgAvailable = computed(() => publicConfig.value?.amneziawg_available ?? false)

const loading = computed(() => meStatus.value === 'pending' || peersStatus.value === 'pending')
const queryError = computed(() => meError.value || peersError.value)
const queryErrorMessage = computed(() => {
  const err = queryError.value
  if (!err) return ''
  if (err instanceof TypeError) return t('common.serverUnavailable')
  return err.message
})

const peers = computed(() => peersData.value?.peers)
const vless = computed(() => peersData.value?.vless)
// False when the server could not query the metrics backend: every counter is then a zero.
const trafficAvailable = computed(() => peersData.value?.traffic_available ?? true)

// Every peer mutation invalidates the list, so the slot counter and the dashboard stay in sync.
const invalidate = useInvalidateQueries()
const invalidatePeers = () => invalidate(getMyPeersQueryKey())
const createMut = useMutation({ ...createMyPeerMutation(), onSettled: invalidatePeers })
const deleteMut = useMutation({ ...deleteMyPeerMutation(), onSettled: invalidatePeers })

const configDialog = ref(false)
const currentConfig = ref<CreatePeerResponse | null>(null)
const currentProtocol = ref('wireguard')

const configDialogTitle = computed(() =>
  t('userPeers.configTitleFor', { protocol: t(`vpn.${currentProtocol.value}`) }),
)

const createDialog = ref(false)
const selectedProtocol = ref<'wireguard' | 'amneziawg'>('wireguard')
const creating = computed(() => createMut.asyncStatus.value === 'loading')
const protocolItems = computed(() => [
  { value: 'wireguard', label: t('vpn.wireguard'), description: t('userPeers.wireguardHint') },
  ...(amneziaWgAvailable.value
    ? [{ value: 'amneziawg', label: t('vpn.amneziawg'), description: t('userPeers.amneziawgHint') }]
    : []),
])

const {
  open: confirmOpen,
  message: confirmMessage,
  request: requestDeletePeer,
  confirm: runDeletePeer,
} = useConfirmAction()

// VLESS state
const vlessDialog = ref(false)
const vlessUri = ref<string | null>(null)
const vlessLoading = ref(false)
const vlessRegenerateConfirmOpen = ref(false)
const regenerateMut = useMutation({
  ...regenerateMyVlessConfigMutation(),
  onSettled: invalidatePeers,
})

// Slots are counted per-device: a client device (which may hold both a WireGuard and an
// AmneziaWG peer) is one slot; each standalone exported config (no device) is one slot.
const slotsUsed = computed(() => {
  if (!peers.value) return 0
  const active = peers.value.filter((p) => p.sync_status !== 'pending_remove')
  const devices = new Set(active.filter((p) => p.device_id).map((p) => p.device_id))
  const standalone = active.filter((p) => !p.device_id).length
  return devices.size + standalone
})

const canCreatePeer = computed(() => {
  if (!me.value?.subscription || !peers.value) return false
  return slotsUsed.value < me.value.subscription.max_peers
})

const peersRemaining = computed(() => {
  if (!me.value?.subscription || !peers.value) return 0
  return me.value.subscription.max_peers - slotsUsed.value
})

const canSubmitConfig = computed(
  () =>
    canCreatePeer.value &&
    !creating.value &&
    regionsStatus.value === 'success' &&
    regionItems.value.some((item) => item.value === configRegion.value) &&
    protocolItems.value.some((item) => item.value === selectedProtocol.value),
)

async function createPeer() {
  if (!canSubmitConfig.value) return
  const protocol = selectedProtocol.value
  try {
    const response = await createMut.mutateAsync({
      body: { protocol, region_id: configRegion.value },
    })
    currentConfig.value = response
    currentProtocol.value = protocol
    createDialog.value = false
    configDialog.value = true
    toast.add({
      title: t('userPeers.configCreated'),
      description: t('userPeers.configCreatedMessage'),
      color: 'success',
    })
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: describeError(e, t('userPeers.createFailed'), t),
      color: 'error',
    })
  }
}

function confirmDeletePeer(peerId: number, peerIp: string) {
  requestDeletePeer(peerId, t('userPeers.deleteConfirm', { ip: peerIp }))
}

async function doDeletePeer() {
  await runDeletePeer(async (id) => {
    try {
      await deleteMut.mutateAsync({ path: { id } })
      toast.add({
        title: t('userPeers.configDeleted'),
        description: t('userPeers.configDeletedMessage'),
        color: 'success',
      })
    } catch (e) {
      toast.add({
        title: t('common.error'),
        description: describeError(e, t('userPeers.deleteFailed'), t),
        color: 'error',
      })
    }
  })
}

async function showConfig(peer: MyPeer) {
  try {
    const { data } = await getMyPeerConfig({ path: { id: peer.id }, throwOnError: true })
    currentConfig.value = {
      id: peer.id,
      assigned_ip: peer.assigned_ip,
      config: data,
    }
    currentProtocol.value = peer.protocol
    configDialog.value = true
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: describeError(e, t('userPeers.createFailed'), t),
      color: 'error',
    })
  }
}

async function copyConfig() {
  if (currentConfig.value) {
    await navigator.clipboard.writeText(currentConfig.value.config)
    toast.add({
      title: t('common.copied'),
      description: t('userPeers.copiedMessage'),
      color: 'success',
    })
  }
}

async function downloadConfig() {
  if (!currentConfig.value) return
  const { id, assigned_ip, config } = currentConfig.value
  const filename = `floppa-vpn-${assigned_ip}.conf`

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
  if (isTauri()) {
    try {
      if (navigator.userAgent.includes('Android')) {
        // Android: save to Download/FloppaVPN/ via MediaStore
        const { createNewPublicFile, writeTextFile, PublicGeneralPurposeDir } =
          await import('tauri-plugin-android-fs-api')
        const uri = await createNewPublicFile(
          PublicGeneralPurposeDir.Download,
          `FloppaVPN/${filename}`,
          'text/plain',
        )
        await writeTextFile(uri, config)
        toast.add({
          title: t('userPeers.configSaved', { path: `Download/FloppaVPN/${filename}` }),
          color: 'success',
        })
      } else {
        // Desktop: save dialog + write file
        const { save } = await import('@tauri-apps/plugin-dialog')
        const { writeTextFile } = await import('@tauri-apps/plugin-fs')
        const path = await save({
          defaultPath: filename,
          filters: [{ name: 'WireGuard', extensions: ['conf'] }],
        })
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

// VLESS functions
async function showVlessConfig() {
  vlessLoading.value = true
  try {
    const { data } = await getMyVlessConfig({ throwOnError: true })
    vlessUri.value = data.uri
    vlessDialog.value = true
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: describeError(e, t('userPeers.vlessLoadFailed'), t),
      color: 'error',
    })
  } finally {
    vlessLoading.value = false
  }
}

async function copyVlessUri() {
  if (vlessUri.value) {
    await navigator.clipboard.writeText(vlessUri.value)
    toast.add({ title: t('userPeers.vlessCopied'), color: 'success' })
  }
}

async function doRegenerateVless() {
  vlessRegenerateConfirmOpen.value = false
  try {
    const response = await regenerateMut.mutateAsync({})
    vlessUri.value = response.uri
    vlessDialog.value = true
    toast.add({ title: t('userPeers.vlessRegenerated'), color: 'success' })
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: describeError(e, t('userPeers.vlessRegenerateFailed'), t),
      color: 'error',
    })
  }
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
        @click="createDialog = true"
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
          <p class="text-sm text-[var(--ui-text-muted)] mt-1">
            {{ t('userPeers.contactSupport') }}
          </p>
        </div>
      </UCard>

      <template v-else>
        <UAlert
          v-if="!trafficAvailable"
          color="neutral"
          variant="subtle"
          icon="i-lucide-bar-chart-3"
          :title="t('traffic.unavailable')"
          class="mb-4"
        />
        <!-- VLESS Section -->
        <UCard v-if="vless" class="mb-6">
          <template #header>
            <div class="flex items-center gap-2 font-semibold">
              <UIcon name="i-lucide-shield" />
              {{ t('userPeers.vlessTitle') }}
            </div>
          </template>
          <div class="flex flex-wrap items-center gap-4">
            <div class="flex gap-6">
              <div class="flex items-center gap-2">
                <UIcon name="i-lucide-arrow-down" class="text-[var(--ui-primary)]" />
                <div>
                  <span class="font-medium">{{
                    formatTraffic(vless.download_bytes, trafficAvailable)
                  }}</span>
                  <span class="ml-1 text-xs text-[var(--ui-text-muted)]">{{
                    t('traffic.download')
                  }}</span>
                </div>
              </div>
              <div class="flex items-center gap-2">
                <UIcon name="i-lucide-arrow-up" class="text-green-500" />
                <div>
                  <span class="font-medium">{{
                    formatTraffic(vless.upload_bytes, trafficAvailable)
                  }}</span>
                  <span class="ml-1 text-xs text-[var(--ui-text-muted)]">{{
                    t('traffic.upload')
                  }}</span>
                </div>
              </div>
            </div>
            <div class="flex gap-2 ml-auto">
              <UButton
                :label="t('userPeers.vlessShowConfig')"
                icon="i-lucide-eye"
                size="sm"
                :loading="vlessLoading"
                @click="showVlessConfig"
              />
              <UButton
                v-if="vless.has_uuid"
                :label="t('userPeers.vlessRegenerate')"
                icon="i-lucide-refresh-cw"
                color="warning"
                variant="soft"
                size="sm"
                @click="() => void (vlessRegenerateConfirmOpen = true)"
              />
            </div>
          </div>
        </UCard>

        <!-- Empty state (no WG peers and no VLESS) -->
        <UCard v-if="!peers?.length && !vless?.has_uuid" class="max-w-sm mx-auto">
          <div class="flex flex-col items-center text-center py-8">
            <UIcon name="i-lucide-key" class="text-4xl text-[var(--ui-text-muted)] mb-4" />
            <p>{{ t('userPeers.noConfigs') }}</p>
            <p class="text-sm text-[var(--ui-text-muted)] mt-1">
              {{ t('userPeers.noConfigsHint') }}
            </p>
          </div>
        </UCard>

        <!-- Peers grid -->
        <div
          v-if="peers?.length"
          class="grid grid-cols-[repeat(auto-fill,minmax(300px,1fr))] gap-4"
        >
          <UCard v-for="peer in peers" :key="peer.id">
            <div class="flex justify-between items-center mb-2">
              <span class="font-mono text-lg font-semibold">{{ peer.assigned_ip }}</span>
              <div class="flex items-center gap-2">
                <UBadge color="neutral" variant="subtle" size="sm">
                  {{ t(`vpn.${peer.protocol}`) }}
                </UBadge>
                <UBadge color="neutral" variant="subtle" size="sm">
                  {{ t(`vpn.regions.${peer.region_id}`, peer.region_id) }}
                </UBadge>
                <StatusBadge :status="peer.sync_status" />
              </div>
            </div>
            <div
              v-if="peer.device_name"
              class="flex items-center gap-1.5 mb-3 text-sm text-[var(--ui-text-muted)]"
            >
              <UIcon name="i-lucide-monitor-smartphone" class="size-4" />
              <span>{{ t('userPeers.device', { name: peer.device_name }) }}</span>
            </div>
            <div class="flex gap-6 mb-3">
              <div class="flex items-center gap-2">
                <UIcon name="i-lucide-arrow-down" class="text-[var(--ui-primary)]" />
                <div>
                  <span class="font-medium">{{
                    formatTraffic(peer.download_bytes, trafficAvailable)
                  }}</span>
                  <span class="ml-1 text-xs text-[var(--ui-text-muted)]">{{
                    t('traffic.download')
                  }}</span>
                </div>
              </div>
              <div class="flex items-center gap-2">
                <UIcon name="i-lucide-arrow-up" class="text-green-500" />
                <div>
                  <span class="font-medium">{{
                    formatTraffic(peer.upload_bytes, trafficAvailable)
                  }}</span>
                  <span class="ml-1 text-xs text-[var(--ui-text-muted)]">{{
                    t('traffic.upload')
                  }}</span>
                </div>
              </div>
            </div>
            <div class="flex flex-col gap-1 text-sm text-[var(--ui-text-muted)] mb-4">
              <span v-if="peer.last_handshake">{{
                t('userPeers.lastSeen', { date: formatDateTime(peer.last_handshake) })
              }}</span>
              <span v-else>{{ t('common.neverConnected') }}</span>
              <span>{{ t('userPeers.createdAt', { date: formatDate(peer.created_at) }) }}</span>
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
                :aria-label="t('common.delete')"
                color="error"
                variant="ghost"
                size="sm"
                @click="confirmDeletePeer(peer.id, peer.assigned_ip)"
              />
            </div>
          </UCard>
        </div>
      </template>
    </template>

    <UModal
      v-model:open="createDialog"
      :title="t('userPeers.createNew')"
      :description="t('userPeers.createDescription')"
      :dismissible="!creating"
      :close="!creating"
    >
      <template #body>
        <div class="flex flex-col gap-6">
          <UFormField :label="t('vpn.protocol')">
            <URadioGroup
              v-model="selectedProtocol"
              :items="protocolItems"
              :disabled="creating"
              variant="card"
              class="mt-2"
            />
          </UFormField>
          <UFormField :label="t('userPeers.exitRegion')" :help="t('userPeers.exitRegionHint')">
            <USelect
              v-model="configRegion"
              :items="regionItems"
              :aria-label="t('userPeers.exitRegion')"
              :loading="regionsStatus === 'pending'"
              :disabled="regionsStatus !== 'success' || creating"
              class="w-full"
            />
          </UFormField>
          <UAlert
            v-if="regionsError"
            color="error"
            :title="t('userPeers.regionsLoadFailed')"
            :actions="[{ label: t('vpn.retry'), onClick: () => refreshRegions() }]"
          />
          <UAlert
            v-else-if="regionsStatus === 'success' && !regionItems.length"
            color="warning"
            :title="t('userPeers.noRegions')"
          />
          <p class="text-sm text-[var(--ui-text-muted)]">{{ t('userPeers.configSlotHint') }}</p>
        </div>
      </template>
      <template #footer>
        <div class="flex justify-end gap-3 w-full flex-wrap">
          <UButton
            :label="t('common.cancel')"
            color="neutral"
            variant="outline"
            :disabled="creating"
            @click="createDialog = false"
          />
          <UButton
            :label="t('userPeers.createNew')"
            icon="i-lucide-plus"
            :loading="creating"
            :disabled="!canSubmitConfig"
            @click="createPeer"
          />
        </div>
      </template>
    </UModal>

    <!-- WG Config Dialog -->
    <UModal v-model:open="configDialog" :title="configDialogTitle">
      <template #body>
        <div v-if="currentConfig" class="flex flex-col gap-4">
          <p class="flex items-center gap-2 text-[var(--ui-text-muted)]">
            <UIcon name="i-lucide-globe" />
            {{ t('userPeers.ip', { ip: currentConfig.assigned_ip }) }}
          </p>
          <pre
            class="bg-[var(--ui-bg-inverted)] text-[var(--ui-text-inverted)] p-4 rounded-lg overflow-x-auto text-sm whitespace-pre-wrap break-all"
            >{{ currentConfig.config }}</pre>
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
        <UButton
          :label="t('common.close')"
          color="neutral"
          variant="outline"
          @click="() => void (configDialog = false)"
        />
      </template>
    </UModal>

    <!-- VLESS Config Dialog -->
    <UModal v-model:open="vlessDialog" :title="t('userPeers.vlessTitle')">
      <template #body>
        <div v-if="vlessUri" class="flex flex-col gap-4">
          <pre
            class="bg-[var(--ui-bg-inverted)] text-[var(--ui-text-inverted)] p-4 rounded-lg overflow-x-auto text-sm whitespace-pre-wrap break-all"
            >{{ vlessUri }}</pre>
        </div>
      </template>
      <template #footer>
        <UButton :label="t('common.copy')" icon="i-lucide-copy" @click="copyVlessUri" />
        <UButton
          :label="t('common.close')"
          color="neutral"
          variant="outline"
          @click="() => void (vlessDialog = false)"
        />
      </template>
    </UModal>

    <!-- Confirm Delete Dialog -->
    <UModal v-model:open="confirmOpen" :title="t('userPeers.deleteConfig')">
      <template #body>
        <p>{{ confirmMessage }}</p>
      </template>
      <template #footer>
        <UButton
          :label="t('common.cancel')"
          color="neutral"
          variant="outline"
          @click="() => void (confirmOpen = false)"
        />
        <UButton
          :label="t('common.delete')"
          color="error"
          :loading="deleteMut.asyncStatus.value === 'loading'"
          @click="doDeletePeer"
        />
      </template>
    </UModal>

    <!-- VLESS Regenerate Confirm Dialog -->
    <UModal v-model:open="vlessRegenerateConfirmOpen" :title="t('userPeers.vlessRegenerate')">
      <template #body>
        <p>{{ t('userPeers.vlessRegenerateConfirm') }}</p>
      </template>
      <template #footer>
        <UButton
          :label="t('common.cancel')"
          color="neutral"
          variant="outline"
          @click="() => void (vlessRegenerateConfirmOpen = false)"
        />
        <UButton
          :label="t('userPeers.vlessRegenerate')"
          color="warning"
          :loading="regenerateMut.asyncStatus.value === 'loading'"
          @click="doRegenerateVless"
        />
      </template>
    </UModal>
  </div>
</template>
