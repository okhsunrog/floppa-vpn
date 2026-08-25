<script setup lang="ts">
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import {
  listInstallationsQuery,
  listInstallationsQueryKey,
  deleteInstallationMutation,
} from '../../client/@pinia/colada.gen'
import type { InstallationSummary } from '../../client/types.gen'
import { describeError, formatDateTime } from '../../utils'
import type { TableColumn } from '@nuxt/ui'
import { useAdminList, useConfirmAction } from '../../composables/adminList'
import { useInvalidateQueries } from '../../composables/invalidate'
import AdminListPage from '../../components/AdminListPage.vue'
import ConfirmModal from '../../components/ConfirmModal.vue'

const router = useRouter()
const { t } = useI18n()
const toast = useToast()
const { data: installations, status, error } = useQuery(listInstallationsQuery())
const invalidate = useInvalidateQueries()
const deleteMut = useMutation({
  ...deleteInstallationMutation(),
  onSettled: () => invalidate(listInstallationsQueryKey()),
})

const {
  open: confirmOpen,
  message: confirmMessage,
  request: requestDelete,
  confirm: runDelete,
} = useConfirmAction()

const {
  search,
  filtered: filteredInstallations,
  page,
  paginated: paginatedInstallations,
  pageSize,
} = useAdminList(installations, (i) => [i.username, i.device_name, i.device_id, i.platform])

function confirmDelete(id: number, deviceName: string | null | undefined) {
  requestDelete(id, t('adminInstallations.deleteConfirm', { device: deviceName || id }))
}

async function doDelete() {
  await runDelete(async (id) => {
    try {
      await deleteMut.mutateAsync({ path: { id } })
      toast.add({
        title: t('common.success'),
        description: t('adminInstallations.deleted'),
        color: 'success',
      })
    } catch (e) {
      toast.add({
        title: t('common.error'),
        description: describeError(e, t('adminInstallations.deleteFailed'), t),
        color: 'error',
      })
    }
  })
}

function openUser(inst: InstallationSummary) {
  void router.push(`/admin/users/${inst.user_id}`)
}

const columns = computed<TableColumn<InstallationSummary>[]>(() => [
  { accessorKey: 'username', header: t('adminInstallations.user') },
  { accessorKey: 'device_name', header: t('adminInstallations.device') },
  { accessorKey: 'platform', header: t('adminInstallations.platform') },
  { accessorKey: 'app_version', header: t('adminInstallations.version') },
  { accessorKey: 'last_seen_at', header: t('adminInstallations.lastSeen') },
  { accessorKey: 'has_wg', header: 'WG' },
  { accessorKey: 'has_vless', header: 'VLESS' },
  { id: 'actions', header: '' },
])
</script>

<template>
  <AdminListPage
    v-model:search="search"
    v-model:page="page"
    :title="t('adminInstallations.title')"
    :status="status"
    :error="error"
    :columns="columns"
    :rows="paginatedInstallations"
    :total="filteredInstallations.length"
    :page-size="pageSize"
    :row-key="(inst) => inst.id"
    :search-placeholder="t('adminInstallations.searchPlaceholder')"
    :empty-text="t('adminInstallations.noInstallations')"
    container-class="max-w-6xl"
    @select="openUser"
  >
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
    <template #platform-cell="{ row }">
      <span v-if="row.original.platform" class="text-sm">{{ row.original.platform }}</span>
      <span v-else class="text-[var(--ui-text-muted)]">-</span>
    </template>
    <template #app_version-cell="{ row }">
      <span v-if="row.original.app_version" class="font-mono text-xs">{{
        row.original.app_version
      }}</span>
      <span v-else class="text-[var(--ui-text-muted)]">-</span>
    </template>
    <template #last_seen_at-cell="{ row }">
      {{ formatDateTime(row.original.last_seen_at) }}
    </template>
    <template #has_wg-cell="{ row }">
      <UIcon
        :name="row.original.has_wg ? 'i-lucide-check' : 'i-lucide-x'"
        :class="row.original.has_wg ? 'text-green-500' : 'text-[var(--ui-text-muted)]'"
        class="size-4"
      />
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
        @click.stop="confirmDelete(row.original.id, row.original.device_name)"
      />
    </template>

    <template #card="{ row: inst }">
      <div class="flex justify-between items-start">
        <div>
          <span class="font-medium">{{ inst.username || '-' }}</span>
          <span
            v-if="inst.device_name"
            class="flex items-center gap-1.5 text-sm text-[var(--ui-text-muted)] mt-0.5"
          >
            <UIcon name="i-lucide-monitor-smartphone" class="size-4" />
            {{ inst.device_name }}
          </span>
        </div>
        <UButton
          icon="i-lucide-trash-2"
          color="error"
          variant="ghost"
          size="xs"
          @click.stop="confirmDelete(inst.id, inst.device_name)"
        />
      </div>
      <div class="flex gap-3 mt-2 text-xs text-[var(--ui-text-muted)] flex-wrap items-center">
        <span v-if="inst.platform">{{ inst.platform }}</span>
        <span v-if="inst.app_version" class="font-mono">v{{ inst.app_version }}</span>
        <span class="flex items-center gap-1">
          WG
          <UIcon
            :name="inst.has_wg ? 'i-lucide-check' : 'i-lucide-x'"
            :class="inst.has_wg ? 'text-green-500' : ''"
            class="size-3.5"
          />
        </span>
        <span class="flex items-center gap-1">
          VLESS
          <UIcon
            :name="inst.has_vless ? 'i-lucide-check' : 'i-lucide-x'"
            :class="inst.has_vless ? 'text-green-500' : ''"
            class="size-3.5"
          />
        </span>
        <span>{{ formatDateTime(inst.last_seen_at) }}</span>
      </div>
    </template>

    <ConfirmModal
      v-model:open="confirmOpen"
      :title="t('adminInstallations.deleteTitle')"
      :message="confirmMessage"
      :confirm-label="t('common.delete')"
      confirm-color="error"
      :loading="deleteMut.asyncStatus.value === 'loading'"
      @confirm="doDelete"
    />
  </AdminListPage>
</template>
