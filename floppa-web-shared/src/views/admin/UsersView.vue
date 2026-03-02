<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import { listUsersQuery, listPlansQuery, createUserMutation } from '../../client/@pinia/colada.gen'
import type { UserSummary } from '../../client/types.gen'
import type { TableColumn } from '@nuxt/ui'

const router = useRouter()
const { t } = useI18n()
const toast = useToast()
const { data: users, status, error, refresh: refreshUsers } = useQuery(listUsersQuery())
const { data: plans } = useQuery(listPlansQuery())
const search = ref('')

const filteredUsers = computed(() => {
  if (!users.value) return []
  if (!search.value) return users.value
  const q = search.value.toLowerCase()
  return users.value.filter(u =>
    (u.username?.toLowerCase().includes(q)) ||
    (u.first_name?.toLowerCase().includes(q)) ||
    (u.last_name?.toLowerCase().includes(q)) ||
    u.telegram_id.toString().includes(q)
  )
})

function displayName(u: UserSummary): string {
  if (u.first_name) return u.last_name ? `${u.first_name} ${u.last_name}` : u.first_name
  return u.username || '-'
}

function formatDate(date: string): string {
  return new Date(date).toLocaleDateString()
}

const columns = computed<TableColumn<UserSummary>[]>(() => [
  { accessorKey: 'id', header: t('adminUsers.id') },
  { accessorKey: 'username', header: t('adminUsers.username') },
  { accessorKey: 'telegram_id', header: t('adminUsers.telegramId') },
  { accessorKey: 'active_plan', header: t('adminUsers.plan') },
  { accessorKey: 'peer_count', header: t('adminUsers.peers') },
  { accessorKey: 'is_admin', header: t('adminUsers.admin') },
  { accessorKey: 'created_at', header: t('adminUsers.created') },
])

// Add User dialog
const addUserDialog = ref(false)
const addUserTelegramId = ref<number | undefined>(undefined)
const addUserName = ref('')
const addUserPlanId = ref<number | undefined>(undefined)
const addUserDays = ref(30)
const addUserPermanent = ref(false)

const addUserMut = useMutation(createUserMutation())

const planItems = computed(() =>
  (plans.value ?? []).map(p => ({ label: p.display_name, value: p.id }))
)

// Set default plan when plans load
watch(plans, (plansData) => {
  if (plansData && plansData.length > 0 && addUserPlanId.value === undefined) {
    const standardPlan = plansData.find(p => p.name === 'standard')
    addUserPlanId.value = standardPlan?.id ?? plansData[0]?.id
  }
})

// Auto-fill days from trial_days when a trial plan is selected
watch(addUserPlanId, (planId) => {
  if (!planId || !plans.value) return
  const plan = plans.value.find(p => p.id === planId)
  if (plan?.trial_days) {
    addUserDays.value = plan.trial_days
  }
})

function openAddUserDialog() {
  addUserTelegramId.value = undefined
  addUserName.value = ''
  addUserDays.value = 30
  addUserPermanent.value = false
  addUserDialog.value = true
}

async function addUser() {
  if (!addUserTelegramId.value || !addUserPlanId.value) return
  try {
    await addUserMut.mutateAsync({
      body: {
        telegram_id: addUserTelegramId.value,
        first_name: addUserName.value || undefined,
        plan_id: addUserPlanId.value,
        days: addUserPermanent.value ? undefined : addUserDays.value,
        permanent: addUserPermanent.value,
      },
    })
    await refreshUsers()
    addUserDialog.value = false
    toast.add({ title: t('common.success'), description: t('adminUsers.userCreated'), color: 'success' })
  } catch (e) {
    const msg = (e instanceof Error && e.message.includes('409'))
      ? t('adminUsers.userExists')
      : (e instanceof Error ? e.message : t('adminUsers.createFailed'))
    toast.add({ title: t('common.error'), description: msg, color: 'error' })
  }
}
</script>

<template>
  <div class="max-w-5xl mx-auto">
    <div class="flex justify-between items-center mb-6 flex-wrap gap-4">
      <h1 class="text-2xl font-bold">{{ t('adminUsers.title') }}</h1>
      <div class="flex items-center gap-2 w-full sm:w-auto">
        <UInput v-model="search" :placeholder="t('adminUsers.searchPlaceholder')" icon="i-lucide-search" class="flex-1 sm:w-64" />
        <UButton :label="t('adminUsers.addUser')" icon="i-lucide-user-plus" size="sm" @click="openAddUserDialog" />
      </div>
    </div>

    <div v-if="status === 'pending'" class="flex justify-center py-12">
      <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
    </div>
    <UAlert v-else-if="error" color="error" :title="error.message" />
    <template v-else>
    <!-- Desktop table -->
    <div class="hidden md:block">
      <UTable
        :data="filteredUsers"
        :columns="columns"
        @select="(_e: Event, row: any) => router.push(`/admin/users/${row.original.id}`)"
        class="[&_tbody_tr]:cursor-pointer"
      >
        <template #username-cell="{ row }">
          <div class="flex items-center gap-2">
            <UAvatar
              :src="row.original.photo_url ?? undefined"
              :alt="displayName(row.original)"
              icon="i-lucide-user"
              size="2xs"
            />
            <div class="flex flex-col">
              <span>{{ displayName(row.original) }}</span>
              <span v-if="row.original.first_name && row.original.username" class="text-xs text-[var(--ui-text-muted)]">@{{ row.original.username }}</span>
            </div>
          </div>
        </template>
        <template #active_plan-cell="{ row }">
          <UBadge v-if="row.original.active_plan" color="success" :label="row.original.active_plan" variant="subtle" />
          <span v-else class="text-[var(--ui-text-muted)]">&mdash;</span>
        </template>
        <template #is_admin-cell="{ row }">
          <UBadge v-if="row.original.is_admin" color="info" :label="t('common.admin')" variant="subtle" />
        </template>
        <template #created_at-cell="{ row }">
          {{ formatDate(row.original.created_at) }}
        </template>
        <template #empty>
          <div class="text-center py-8 text-[var(--ui-text-muted)]">{{ t('adminUsers.noUsers') }}</div>
        </template>
      </UTable>
    </div>

    <!-- Mobile cards -->
    <div class="md:hidden flex flex-col gap-3">
      <div v-if="filteredUsers.length === 0" class="text-center py-8 text-[var(--ui-text-muted)]">
        {{ t('adminUsers.noUsers') }}
      </div>
      <UCard
        v-for="user in filteredUsers"
        :key="user.id"
        class="cursor-pointer active:scale-[0.98] transition-transform"
        @click="router.push(`/admin/users/${user.id}`)"
      >
        <div class="flex justify-between items-start">
          <div class="flex items-center gap-2">
            <UAvatar
              :src="user.photo_url ?? undefined"
              :alt="displayName(user)"
              icon="i-lucide-user"
              size="2xs"
            />
            <div>
              <span class="font-medium">{{ displayName(user) }}</span>
              <span v-if="user.first_name && user.username" class="block text-xs text-[var(--ui-text-muted)]">@{{ user.username }}</span>
              <span class="block text-xs text-[var(--ui-text-muted)]">ID: {{ user.id }} &middot; TG: {{ user.telegram_id }}</span>
            </div>
          </div>
          <div class="flex gap-1">
            <UBadge v-if="user.is_admin" color="info" label="Admin" variant="subtle" size="sm" />
            <UBadge v-if="user.active_plan" color="success" :label="user.active_plan" variant="subtle" size="sm" />
            <span v-else class="text-xs text-[var(--ui-text-muted)]">&mdash;</span>
          </div>
        </div>
        <div class="flex gap-4 mt-2 text-xs text-[var(--ui-text-muted)]">
          <span>{{ t('adminUsers.peers') }}: {{ user.peer_count }}</span>
          <span>{{ formatDate(user.created_at) }}</span>
        </div>
      </UCard>
    </div>
    </template>

    <!-- Add User Dialog -->
    <UModal v-model:open="addUserDialog" :title="t('adminUsers.addUserTitle')">
      <template #body>
        <div class="flex flex-col gap-4">
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Telegram ID *</label>
            <UInput type="number" v-model.number="addUserTelegramId" placeholder="123456789" />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUsers.nameLabel') }}</label>
            <UInput v-model="addUserName" :placeholder="t('adminUsers.namePlaceholder')" />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUserDetail.plan') }}</label>
            <USelect v-model="addUserPlanId" :items="planItems" :placeholder="t('adminUserDetail.selectPlan')" />
          </div>
          <div class="flex items-center gap-2">
            <UCheckbox v-model="addUserPermanent" />
            <label class="text-sm font-medium">{{ t('adminUserDetail.permanent') }}</label>
          </div>
          <div v-if="!addUserPermanent" class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUserDetail.days') }}</label>
            <UInput type="number" v-model.number="addUserDays" :min="1" />
          </div>
        </div>
      </template>
      <template #footer>
        <UButton :label="t('common.cancel')" color="neutral" variant="outline" @click="addUserDialog = false" />
        <UButton :label="t('common.create')" :loading="addUserMut.asyncStatus.value === 'loading'" :disabled="!addUserTelegramId || !addUserPlanId" @click="addUser" />
      </template>
    </UModal>
  </div>
</template>
