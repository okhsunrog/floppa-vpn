<script setup lang="ts">
import { ref, computed } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import { listInstallationsQuery, deleteInstallationMutation } from '../../client/@pinia/colada.gen'
import type { InstallationSummary } from '../../client/types.gen'
import { formatDateTime } from '../../utils'
import type { TableColumn } from '@nuxt/ui'

const router = useRouter()
const { t } = useI18n()
const toast = useToast()
const {
  data: installations,
  status,
  error,
  refresh: refreshInstallations,
} = useQuery(listInstallationsQuery())
const deleteMut = useMutation(deleteInstallationMutation())
const search = ref('')

const confirmOpen = ref(false)
const confirmMessage = ref('')
const pendingId = ref<number | null>(null)

const filteredInstallations = computed(() => {
  if (!installations.value) return []
  if (!search.value) return installations.value
  const q = search.value.toLowerCase()
  return installations.value.filter(
    (i) =>
      i.username?.toLowerCase().includes(q) ||
      i.device_name?.toLowerCase().includes(q) ||
      i.device_id.toLowerCase().includes(q) ||
      i.platform?.toLowerCase().includes(q),
  )
})

function confirmDelete(id: number, deviceName: string | null | undefined) {
  pendingId.value = id
  confirmMessage.value = t('adminInstallations.deleteConfirm', {
    device: deviceName || id,
  })
  confirmOpen.value = true
}

async function doDelete() {
  if (!pendingId.value) return
  try {
    await deleteMut.mutateAsync({ path: { id: pendingId.value } })
    await refreshInstallations()
    toast.add({
      title: t('common.success'),
      description: t('adminInstallations.deleted'),
      color: 'success',
    })
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: e instanceof Error ? e.message : t('adminInstallations.deleteFailed'),
      color: 'error',
    })
  }
  confirmOpen.value = false
  pendingId.value = null
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
  <div class="max-w-6xl mx-auto">
    <div class="flex justify-between items-center mb-6 flex-wrap gap-4">
      <h1 class="text-2xl font-bold">{{ t('adminInstallations.title') }}</h1>
      <UInput
        v-model="search"
        :placeholder="t('adminInstallations.searchPlaceholder')"
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
          :data="filteredInstallations"
          :columns="columns"
          class="[&_tbody_tr]:cursor-pointer"
          @select="(_e: Event, row: any) => router.push(`/admin/users/${row.original.user_id}`)"
        >
          <template #username-cell="{ row }">
            {{ row.original.username || '-' }}
          </template>
          <template #device_name-cell="{ row }">
            <span v-if="row.original.device_name" class="flex items-center gap-1.5">
              <UIcon
                name="i-lucide-monitor-smartphone"
                class="size-4 text-[var(--ui-text-muted)]"
              />
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
          <template #empty>
            <div class="text-center py-8 text-[var(--ui-text-muted)]">
              {{ t('adminInstallations.noInstallations') }}
            </div>
          </template>
        </UTable>
      </div>

      <!-- Mobile cards -->
      <div class="md:hidden flex flex-col gap-3">
        <div
          v-if="filteredInstallations.length === 0"
          class="text-center py-8 text-[var(--ui-text-muted)]"
        >
          {{ t('adminInstallations.noInstallations') }}
        </div>
        <UCard
          v-for="inst in filteredInstallations"
          :key="inst.id"
          class="cursor-pointer active:scale-[0.98] transition-transform"
          @click="router.push(`/admin/users/${inst.user_id}`)"
        >
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
        </UCard>
      </div>
    </template>

    <!-- Confirm Delete Dialog -->
    <UModal v-model:open="confirmOpen" :title="t('adminInstallations.deleteTitle')">
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
          :label="t('common.delete')"
          color="error"
          :loading="deleteMut.asyncStatus.value === 'loading'"
          @click="doDelete"
        />
      </template>
    </UModal>
  </div>
</template>
