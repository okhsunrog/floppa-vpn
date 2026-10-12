<script setup lang="ts">
import { ref, computed, watch, h } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import {
  listUsersQuery,
  listUsersQueryKey,
  getStatsQueryKey,
  listPlansQuery,
  createUserMutation,
} from '../../client/@pinia/colada.gen'
import { getAvatarsBatch } from '../../client/sdk.gen'
import type { UserSummary } from '../../client/types.gen'
import type { TableColumn } from '@nuxt/ui'
import { describeError, formatDate, isApiError, toIntOrNull } from '../../utils'
import { useAdminList } from '../../composables/adminList'
import { useInvalidateQueries } from '../../composables/invalidate'
import AdminListPage from '../../components/AdminListPage.vue'
import { selectAdminUsers, type SubscriptionFilter } from '../../composables/adminUsers'

const props = withDefaults(defineProps<{ embedded?: boolean; activeOnly?: boolean }>(), {
  embedded: false,
  activeOnly: false,
})

const router = useRouter()
const { t, locale } = useI18n()
const route = useRoute()
const toast = useToast()
const { data: allUsers, status, error } = useQuery(listUsersQuery())
function queryValue(key: string, fallback: string) {
  return typeof route.query[key] === 'string' ? (route.query[key] as string) : fallback
}
function setQuery(key: string, value: string) {
  if (queryValue(key, '') === value) return
  void router.replace({ query: { ...route.query, [key]: value || undefined } })
}
const planFilter = computed({
  get: () => queryValue('plan', 'all'),
  set: (v: string) => setQuery('plan', v),
})
const subscriptionFilter = computed({
  get: (): SubscriptionFilter => {
    const value = queryValue('subscription', props.activeOnly ? 'active' : 'all')
    return ['all', 'active', 'expired', 'none'].includes(value)
      ? (value as SubscriptionFilter)
      : 'all'
  },
  set: (v: SubscriptionFilter) => setQuery('subscription', v),
})
const sortKey = computed({
  get: () => queryValue('sort', props.activeOnly ? 'subscription_expires_at' : 'created_at'),
  set: (v: string) => setQuery('sort', v),
})
const descending = computed(() => queryValue('order', props.activeOnly ? 'asc' : 'desc') === 'desc')
const expiring = computed(() => route.query.expires === 'week')
const users = computed(() =>
  selectAdminUsers(allUsers.value ?? [], {
    plan: planFilter.value,
    subscription: subscriptionFilter.value,
    expiring: expiring.value,
    sort: sortKey.value,
    descending: descending.value,
    now: Date.now(),
    locale: locale.value,
  }),
)
const filterPlanItems = computed(() => [
  { label: t('adminWorkspace.allPlans'), value: 'all' },
  ...(plans.value ?? []).map((p) => ({ label: p.display_name, value: String(p.id) })),
])
const statusItems = computed(() =>
  ['all', 'active', 'expired', 'none'].map((value) => ({
    value,
    label: t(`adminWorkspace.${value}`),
  })),
)
function changeSort(key: string) {
  void router.replace({
    query: {
      ...route.query,
      sort: key,
      order: sortKey.value === key && !descending.value ? 'desc' : 'asc',
    },
  })
}
function clearFilters() {
  const query = { ...route.query }
  for (const key of ['q', 'plan', 'subscription', 'expires', 'sort', 'order']) delete query[key]
  void router.replace({ query })
}
function subscriptionLabel(user: UserSummary) {
  return t(
    `adminWorkspace.${user.current_plan_id == null ? 'none' : user.subscription_active ? 'active' : 'expired'}`,
  )
}
function expiryLabel(user: UserSummary) {
  return user.current_plan_id == null
    ? '—'
    : user.subscription_expires_at
      ? formatDate(user.subscription_expires_at)
      : t('adminPlans.permanent')
}
const invalidate = useInvalidateQueries()
const { data: plans } = useQuery(listPlansQuery())

const {
  search,
  filtered: filteredUsers,
  page,
  paginated: paginatedUsers,
  pageSize,
} = useAdminList(
  users,
  (u) => [u.id, u.username, u.first_name, u.last_name, u.telegram_id],
  props.embedded ? 10 : 20,
)

search.value = queryValue('q', '')
function updateSearch(value: string) {
  search.value = value
  setQuery('q', value)
}
watch(
  () => route.query.q,
  () => {
    search.value = queryValue('q', '')
  },
)
watch([planFilter, subscriptionFilter, sortKey, descending, expiring], () => {
  page.value = 1
})

function openUser(user: UserSummary) {
  void router.push(`/admin/users/${user.id}`)
}

function displayName(u: UserSummary): string {
  if (u.first_name) return u.last_name ? `${u.first_name} ${u.last_name}` : u.first_name
  return u.username || '-'
}

// Server-cached avatars as data URLs, keyed by user id (Telegram CDN is unreachable from clients).
// Fetched in one batch when the user list loads.
const avatars = ref<Record<string, string>>({})
// Fetch avatars only for the currently visible page (and only those not already cached), so the
// base64 payload stays bounded regardless of total user count.
watch(
  () => props.activeOnly,
  () => {
    page.value = 1
  },
)

watch(
  paginatedUsers,
  async (list) => {
    const missing = list.filter((u) => !(String(u.id) in avatars.value)).map((u) => u.id)
    if (!missing.length) return
    try {
      const { data } = await getAvatarsBatch({ body: { user_ids: missing } })
      if (data) avatars.value = { ...avatars.value, ...data }
    } catch {
      // best-effort
    }
  },
  { immediate: true },
)

const allColumns = computed<TableColumn<UserSummary>[]>(() => [
  { accessorKey: 'id', header: t('adminUsers.id') },
  { accessorKey: 'username', header: t('adminUsers.username') },
  { accessorKey: 'telegram_id', header: t('adminUsers.telegramId') },
  { accessorKey: 'current_plan', header: t('adminUsers.plan') },
  { accessorKey: 'subscription_expires_at', header: t('adminWorkspace.expires') },
  { accessorKey: 'peer_count', header: t('adminUsers.peers') },
  { accessorKey: 'has_vless', header: 'VLESS' },
  { accessorKey: 'client_version', header: t('adminUsers.version') },
  { accessorKey: 'is_admin', header: t('adminUsers.admin') },
  { accessorKey: 'created_at', header: t('adminUsers.created') },
])

const visibleColumns = computed(() =>
  props.embedded || props.activeOnly
    ? allColumns.value.filter(
        (c) =>
          'accessorKey' in c &&
          [
            'username',
            'current_plan',
            'subscription_expires_at',
            'peer_count',
            'client_version',
          ].includes(String(c.accessorKey)),
      )
    : allColumns.value,
)

const columns = computed<TableColumn<UserSummary>[]>(() =>
  visibleColumns.value.map((column) => {
    if (!('accessorKey' in column)) return column
    const key = String(column.accessorKey)
    return {
      ...column,
      header: () =>
        h(
          'button',
          {
            type: 'button',
            class:
              'inline-flex items-center gap-2 py-2 text-left font-medium hover:text-[var(--ui-text)] focus-visible:outline-2',
            'aria-label': `${t('adminWorkspace.sort')}: ${column.header}`,
            'aria-pressed': sortKey.value === key,
            onClick: () => changeSort(key),
          },
          `${column.header} ${sortKey.value === key ? (descending.value ? '↓' : '↑') : '↕'}`,
        ),
    }
  }),
)
const sortItems = computed(() =>
  allColumns.value
    .filter((c) => 'accessorKey' in c)
    .map((c) => ({
      label: String(c.header),
      value: String('accessorKey' in c ? c.accessorKey : ''),
    })),
)

// Grant subscriptions to existing users, independently of the list's active filters.
const recipientDialog = ref(false)
const recipientSearch = ref('')
const recipients = computed(() => {
  const query = recipientSearch.value.trim().toLocaleLowerCase()
  return (allUsers.value ?? []).filter((user) =>
    [displayName(user), user.username, user.id, user.telegram_id].some((value) =>
      String(value ?? '')
        .toLocaleLowerCase()
        .includes(query),
    ),
  )
})
function openRecipientDialog() {
  recipientSearch.value = ''
  recipientDialog.value = true
}
function selectRecipient(user: UserSummary) {
  recipientDialog.value = false
  void router.push({ path: `/admin/users/${user.id}`, query: { action: 'subscription' } })
}

// Pre-register a user who has not signed in yet.
const addUserDialog = ref(false)
const addUserTelegramId = ref('')
const telegramId = computed(() => {
  const value = addUserTelegramId.value.trim()
  const id = Number(value)
  return /^\d+$/.test(value) && Number.isSafeInteger(id) && id > 0 ? id : null
})
const addUserName = ref('')
const addUserPlanId = ref<number | undefined>(undefined)
const addUserDays = ref<number | null>(30)
const addUserPermanent = ref(false)

const addUserValid = computed(
  () =>
    telegramId.value !== null &&
    addUserPlanId.value !== undefined &&
    (addUserPermanent.value || (addUserDays.value !== null && addUserDays.value >= 1)),
)

const addUserMut = useMutation({
  ...createUserMutation(),
  onSettled: () => invalidate(listUsersQueryKey(), getStatsQueryKey()),
})

const planItems = computed(() =>
  (plans.value ?? []).map((p) => ({ label: p.display_name, value: p.id })),
)

// Set default plan when plans load
watch(plans, (plansData) => {
  if (plansData && plansData.length > 0 && addUserPlanId.value === undefined) {
    const standardPlan = plansData.find((p) => p.name === 'standard')
    addUserPlanId.value = standardPlan?.id ?? plansData[0]?.id
  }
})

// Auto-fill days from the plan's trial duration (minutes) when a trial plan is selected.
// The admin field is day-granular, so sub-day trials (e.g. taster) leave it untouched.
watch(addUserPlanId, (planId) => {
  if (!planId || !plans.value) return
  const plan = plans.value.find((p) => p.id === planId)
  if (plan?.trial_minutes) {
    const days = Math.round(plan.trial_minutes / 1440)
    if (days >= 1) addUserDays.value = days
  }
})

function openAddUserDialog() {
  addUserTelegramId.value = ''
  addUserName.value = ''
  addUserPlanId.value =
    plans.value?.find((plan) => plan.name === 'standard')?.id ?? plans.value?.[0]?.id
  addUserDays.value = 30
  addUserPermanent.value = false
  addUserDialog.value = true
}

async function addUser() {
  if (!addUserValid.value || telegramId.value === null || !addUserPlanId.value) return
  try {
    await addUserMut.mutateAsync({
      body: {
        telegram_id: telegramId.value,
        first_name: addUserName.value || undefined,
        plan_id: addUserPlanId.value,
        days: addUserPermanent.value ? undefined : (addUserDays.value ?? undefined),
        permanent: addUserPermanent.value,
      },
    })
    addUserDialog.value = false
    toast.add({
      title: t('common.success'),
      description: t('adminUsers.userCreated'),
      color: 'success',
    })
  } catch (e) {
    const msg =
      isApiError(e) && e.error === 'conflict'
        ? t('adminUsers.userExists')
        : describeError(e, t('adminUsers.createFailed'), t)
    toast.add({ title: t('common.error'), description: msg, color: 'error' })
  }
}
</script>

<template>
  <AdminListPage
    :search="search"
    @update:search="updateSearch"
    v-model:page="page"
    :title="t(activeOnly ? 'adminWorkspace.subscriptions' : 'adminUsers.title')"
    :heading-tag="embedded ? 'h2' : 'h1'"
    :status="status"
    :error="error"
    :columns="columns"
    :rows="paginatedUsers"
    :total="filteredUsers.length"
    :page-size="pageSize"
    :row-key="(user) => user.id"
    :search-placeholder="t('adminUsers.searchPlaceholder')"
    :empty-text="t('adminUsers.noUsers')"
    @select="openUser"
  >
    <template #header-actions>
      <UButton
        :label="t(activeOnly ? 'adminUsers.grantSubscription' : 'adminUsers.addUser')"
        :icon="activeOnly ? 'i-lucide-ticket-plus' : 'i-lucide-user-plus'"
        size="lg"
        @click="activeOnly ? openRecipientDialog() : openAddUserDialog()"
      />
    </template>

    <template #filters>
      <div
        class="grid grid-cols-1 min-[360px]:grid-cols-2 gap-3 sm:flex sm:flex-wrap sm:items-center"
      >
        <UFormField :label="t('adminUsers.plan')" class="min-w-0 sm:w-48">
          <USelect
            v-model="planFilter"
            :items="filterPlanItems"
            :aria-label="t('adminUsers.plan')"
            class="w-full"
            size="lg"
          />
        </UFormField>
        <UFormField :label="t('adminWorkspace.status')" class="min-w-0 sm:w-44">
          <USelect
            v-model="subscriptionFilter"
            :items="statusItems"
            :aria-label="t('adminWorkspace.status')"
            class="w-full"
            size="lg"
          />
        </UFormField>
        <UButton
          v-if="
            planFilter !== 'all' ||
            expiring ||
            search ||
            subscriptionFilter !== (activeOnly ? 'active' : 'all')
          "
          :label="t('adminWorkspace.clear')"
          icon="i-lucide-x"
          variant="ghost"
          color="neutral"
          class="self-end"
          @click="clearFilters"
        />
      </div>
      <div class="flex flex-wrap justify-between gap-2 mt-3 text-sm text-[var(--ui-text-muted)]">
        <span aria-live="polite">{{
          t('adminWorkspace.found', { count: filteredUsers.length })
        }}</span>
        <span v-if="expiring">{{ t('adminWorkspace.expiring') }}</span>
      </div>
      <div class="flex items-end gap-2 mt-3 md:hidden">
        <UFormField :label="t('adminWorkspace.sort')" class="min-w-0 flex-1"
          ><USelect
            v-model="sortKey"
            :items="sortItems"
            class="w-full"
            size="lg"
            :aria-label="t('adminWorkspace.sort')"
        /></UFormField>
        <UButton
          :icon="descending ? 'i-lucide-arrow-down-wide-narrow' : 'i-lucide-arrow-up-narrow-wide'"
          color="neutral"
          variant="outline"
          :aria-label="t('adminWorkspace.reverseSort')"
          @click="setQuery('order', descending ? 'asc' : 'desc')"
        />
      </div>
    </template>

    <template #username-cell="{ row }">
      <div class="flex items-center gap-2 min-w-0">
        <UAvatar
          :src="avatars[String(row.original.id)] ?? undefined"
          :alt="displayName(row.original)"
          icon="i-lucide-user"
          size="2xs"
        />
        <div class="flex flex-col">
          <span>{{ displayName(row.original) }}</span>
          <span
            v-if="row.original.first_name && row.original.username"
            class="text-xs text-[var(--ui-text-muted)]"
            >@{{ row.original.username }}</span
          >
        </div>
      </div>
    </template>
    <template #current_plan-cell="{ row }">
      <span class="block font-medium">{{ row.original.current_plan ?? '—' }}</span>
      <span class="text-xs text-[var(--ui-text-muted)]">{{ subscriptionLabel(row.original) }}</span>
    </template>
    <template #subscription_expires_at-cell="{ row }">{{ expiryLabel(row.original) }}</template>
    <template #has_vless-cell="{ row }">
      <UIcon
        :name="row.original.has_vless ? 'i-lucide-check' : 'i-lucide-x'"
        :class="row.original.has_vless ? 'text-green-500' : 'text-[var(--ui-text-muted)]'"
        class="size-4"
      />
    </template>
    <template #client_version-cell="{ row }">
      <span v-if="row.original.client_version" class="text-xs">{{
        row.original.client_version
      }}</span>
      <span v-else class="text-[var(--ui-text-muted)]">&mdash;</span>
    </template>
    <template #is_admin-cell="{ row }">
      <UBadge
        v-if="row.original.is_admin"
        color="info"
        :label="t('common.admin')"
        variant="subtle"
      />
    </template>
    <template #created_at-cell="{ row }">
      {{ formatDate(row.original.created_at) }}
    </template>

    <template #card="{ row: user }">
      <div class="flex justify-between items-start gap-3">
        <div class="flex items-center gap-2 min-w-0">
          <UAvatar
            :src="avatars[String(user.id)] ?? undefined"
            :alt="displayName(user)"
            icon="i-lucide-user"
            size="2xs"
          />
          <div class="min-w-0 [overflow-wrap:anywhere]">
            <span class="font-medium">{{ displayName(user) }}</span>
            <span
              v-if="user.first_name && user.username"
              class="block text-xs text-[var(--ui-text-muted)]"
              >@{{ user.username }}</span
            >
            <span v-if="!embedded" class="block text-xs text-[var(--ui-text-muted)]">{{
              t('adminUsers.idAndTelegram', { id: user.id, tg: user.telegram_id ?? '—' })
            }}</span>
          </div>
        </div>
        <div class="flex flex-col items-end gap-1 shrink-0">
          <UBadge
            v-if="user.is_admin"
            color="info"
            :label="t('common.admin')"
            variant="subtle"
            size="sm"
          />
        </div>
      </div>
      <p v-if="user.current_plan" class="mt-3 text-sm font-medium break-words">
        {{ user.current_plan }}
      </p>
      <p class="mt-1 text-sm text-[var(--ui-text-muted)]">
        {{ subscriptionLabel(user)
        }}<span v-if="user.current_plan_id != null"> · {{ expiryLabel(user) }}</span>
      </p>
      <div
        class="flex flex-wrap gap-x-4 gap-y-2 mt-2 text-xs text-[var(--ui-text-muted)] items-center"
      >
        <span>{{ t('adminUsers.peers') }}: {{ user.peer_count }}</span>
        <span v-if="!embedded" class="flex items-center gap-1">
          VLESS
          <UIcon
            :name="user.has_vless ? 'i-lucide-check' : 'i-lucide-x'"
            :class="user.has_vless ? 'text-green-500' : ''"
            class="size-3.5"
          />
        </span>
        <span v-if="user.client_version">v{{ user.client_version }}</span>
        <span v-if="!embedded">{{ formatDate(user.created_at) }}</span>
      </div>
    </template>

    <UModal
      v-model:open="recipientDialog"
      :title="t('adminUsers.grantSubscription')"
      :description="t('adminUsers.chooseRecipient')"
    >
      <template #body>
        <UInput
          v-model="recipientSearch"
          :placeholder="t('adminUsers.recipientSearch')"
          :aria-label="t('adminUsers.recipientSearch')"
          icon="i-lucide-search"
          autofocus
          size="lg"
          class="w-full mb-4"
        />
        <div class="max-h-72 overflow-y-auto -mx-2">
          <button
            v-for="recipient in recipients.slice(0, 20)"
            :key="recipient.id"
            type="button"
            class="flex w-full items-center gap-3 rounded-lg px-3 py-3 text-left hover:bg-elevated focus-visible:outline-2 focus-visible:outline-primary"
            @click="selectRecipient(recipient)"
          >
            <span class="min-w-0 flex-1">
              <span class="block truncate font-medium">{{ displayName(recipient) }}</span>
              <span class="block truncate text-sm text-muted">
                {{ recipient.username ? `@${recipient.username} · ` : '' }}ID {{ recipient.id }}
                {{ recipient.telegram_id ? ` · Telegram ${recipient.telegram_id}` : '' }}
              </span>
            </span>
            <UIcon name="i-lucide-chevron-right" class="size-4 shrink-0 text-muted" />
          </button>
          <p v-if="!recipients.length" class="p-3 text-muted">{{ t('adminUsers.noUsers') }}</p>
        </div>
        <p v-if="recipients.length > 20" class="mt-3 text-sm text-muted">
          {{ t('adminUsers.refineSearch') }}
        </p>
      </template>
    </UModal>

    <UModal
      v-model:open="addUserDialog"
      :title="t('adminUsers.addUserTitle')"
      :description="t('adminUsers.preRegisterDescription')"
    >
      <template #body>
        <div class="flex flex-col gap-4">
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUsers.telegramId') }} *</label>
            <UInput
              v-model="addUserTelegramId"
              type="text"
              inputmode="numeric"
              :aria-label="t('adminUsers.telegramId')"
              placeholder="123456789"
            />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUsers.nameLabel') }}</label>
            <UInput v-model="addUserName" :placeholder="t('adminUsers.namePlaceholder')" />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUserDetail.plan') }}</label>
            <USelect
              v-model="addUserPlanId"
              :items="planItems"
              :placeholder="t('adminUserDetail.selectPlan')"
            />
          </div>
          <div class="flex items-center gap-2 min-w-0">
            <UCheckbox v-model="addUserPermanent" />
            <label class="text-sm font-medium">{{ t('adminUserDetail.permanent') }}</label>
          </div>
          <div v-if="!addUserPermanent" class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminUserDetail.days') }}</label>
            <UInput
              type="number"
              :model-value="addUserDays ?? undefined"
              @update:model-value="(v: string | number) => (addUserDays = toIntOrNull(v))"
              :min="1"
            />
          </div>
        </div>
      </template>
      <template #footer>
        <UButton
          :label="t('common.cancel')"
          color="neutral"
          variant="outline"
          @click="() => void (addUserDialog = false)"
        />
        <UButton
          :label="t('common.create')"
          :loading="addUserMut.asyncStatus.value === 'loading'"
          :disabled="!addUserValid"
          @click="addUser"
        />
      </template>
    </UModal>
  </AdminListPage>
</template>
