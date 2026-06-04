<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useQuery, useMutation } from '@pinia/colada'
import {
  listPlansQuery,
  createPlanMutation,
  updatePlanMutation,
  deletePlanMutation,
} from '../../client/@pinia/colada.gen'
import type { Plan, CreatePlanRequest, UpdatePlanRequest } from '../../client/types.gen'
import type { TableColumn } from '@nuxt/ui'
import { durationUnit } from '../../utils/format'

const { t } = useI18n()
const toast = useToast()

const { data: plans, status, error, refresh } = useQuery(listPlansQuery())

const createMut = useMutation(createPlanMutation())
const updateMut = useMutation(updatePlanMutation())
const deleteMut = useMutation(deletePlanMutation())

const submitting = computed(
  () =>
    createMut.asyncStatus.value === 'loading' ||
    updateMut.asyncStatus.value === 'loading' ||
    deleteMut.asyncStatus.value === 'loading',
)

const planDialog = ref(false)
const isEditing = ref(false)
const editingPlanId = ref<number | null>(null)
const planForm = ref<CreatePlanRequest>({
  name: '',
  display_name: '',
  default_speed_limit_mbps: null,
  max_peers: 1,
  is_public: true,
  trial_minutes: null,
  price_stars: null,
  period_days: null,
})

// Trial duration lives on the plan in minutes; the form edits it as a value + unit so
// sub-day trials (e.g. the 2h taster) are easy to enter. `planForm.trial_minutes` stays
// in sync via the watcher below and is what gets sent to the API.
const TRIAL_UNIT_FACTOR = { minutes: 1, hours: 60, days: 1440 } as const
const trialValue = ref<number | null>(null)
const trialUnit = ref<'minutes' | 'hours' | 'days'>('days')
watch([trialValue, trialUnit], () => {
  planForm.value.trial_minutes =
    trialValue.value == null ? null : trialValue.value * TRIAL_UNIT_FACTOR[trialUnit.value]
})
const trialUnitItems = computed(() => [
  { label: t('adminPlans.unitMinutes'), value: 'minutes' },
  { label: t('adminPlans.unitHours'), value: 'hours' },
  { label: t('adminPlans.unitDays'), value: 'days' },
])
function formatTrial(min: number): string {
  const { unit, n } = durationUnit(min)
  return t(`adminPlans.${unit}`, { n })
}
function setTrialFromMinutes(min: number | null | undefined) {
  if (min == null) {
    trialValue.value = null
    trialUnit.value = 'days'
  } else {
    const p = durationUnit(min)
    trialValue.value = p.n
    trialUnit.value = p.unit
  }
}

// Confirm delete state
const confirmOpen = ref(false)
const confirmMessage = ref('')
const pendingDeletePlan = ref<Plan | null>(null)

function openNewPlanDialog() {
  isEditing.value = false
  editingPlanId.value = null
  planForm.value = {
    name: '',
    display_name: '',
    default_speed_limit_mbps: null,
    max_peers: 1,
    is_public: true,
    trial_minutes: null,
    price_stars: null,
    period_days: null,
  }
  setTrialFromMinutes(null)
  planDialog.value = true
}

function openEditPlanDialog(plan: Plan) {
  isEditing.value = true
  editingPlanId.value = plan.id
  planForm.value = {
    name: plan.name,
    display_name: plan.display_name,
    default_speed_limit_mbps: plan.default_speed_limit_mbps ?? null,
    max_peers: plan.max_peers,
    is_public: plan.is_public,
    trial_minutes: plan.trial_minutes ?? null,
    price_stars: plan.price_stars ?? null,
    period_days: plan.period_days ?? null,
  }
  setTrialFromMinutes(plan.trial_minutes)
  planDialog.value = true
}

async function savePlan() {
  if (!planForm.value.name || !planForm.value.display_name) {
    toast.add({
      title: t('common.validation'),
      description: t('adminPlans.nameRequired'),
      color: 'warning',
    })
    return
  }
  try {
    if (isEditing.value && editingPlanId.value) {
      const update: UpdatePlanRequest = {
        display_name: planForm.value.display_name,
        default_speed_limit_mbps: planForm.value.default_speed_limit_mbps,
        max_peers: planForm.value.max_peers,
        is_public: planForm.value.is_public,
        trial_minutes: planForm.value.trial_minutes,
        price_stars: planForm.value.price_stars,
        period_days: planForm.value.period_days,
        clear_speed_limit: planForm.value.default_speed_limit_mbps == null,
        clear_trial_minutes: planForm.value.trial_minutes == null,
        clear_price_stars: planForm.value.price_stars == null,
        clear_period_days: planForm.value.period_days == null,
      }
      await updateMut.mutateAsync({
        path: { id: editingPlanId.value },
        body: update,
      })
      // refresh() below re-fetches the list, so no manual cache patch is needed.
      toast.add({
        title: t('common.success'),
        description: t('adminPlans.planUpdated'),
        color: 'success',
      })
    } else {
      await createMut.mutateAsync({ body: planForm.value })
      toast.add({
        title: t('common.success'),
        description: t('adminPlans.planCreated'),
        color: 'success',
      })
    }
    await refresh()
    planDialog.value = false
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: e instanceof Error ? e.message : t('adminPlans.saveFailed'),
      color: 'error',
    })
  }
}

function confirmDeletePlan(plan: Plan) {
  pendingDeletePlan.value = plan
  confirmMessage.value = t('adminPlans.deleteConfirm', { name: plan.display_name })
  confirmOpen.value = true
}

async function doDeletePlan() {
  if (!pendingDeletePlan.value) return
  try {
    await deleteMut.mutateAsync({ path: { id: pendingDeletePlan.value.id } })
    await refresh()
    toast.add({
      title: t('common.success'),
      description: t('adminPlans.planDeleted'),
      color: 'success',
    })
  } catch (e) {
    toast.add({
      title: t('common.error'),
      description: e instanceof Error ? e.message : t('adminPlans.deleteFailed'),
      color: 'error',
    })
  }
  confirmOpen.value = false
  pendingDeletePlan.value = null
}

const columns = computed<TableColumn<Plan>[]>(() => [
  { accessorKey: 'name', header: t('adminPlans.name') },
  { accessorKey: 'display_name', header: t('adminPlans.displayName') },
  { accessorKey: 'default_speed_limit_mbps', header: t('adminPlans.speed') },
  { accessorKey: 'max_peers', header: t('adminPlans.maxPeers') },
  { accessorKey: 'price_stars', header: t('adminPlans.price') },
  { accessorKey: 'period_days', header: t('adminPlans.period') },
  { accessorKey: 'trial_minutes', header: t('adminPlans.trial') },
  { accessorKey: 'is_public', header: t('adminPlans.public') },
  { id: 'actions', header: t('adminPlans.actions') },
])
</script>

<template>
  <div class="max-w-5xl mx-auto">
    <div class="flex justify-between items-center mb-6">
      <h1 class="text-2xl font-bold">{{ t('adminPlans.title') }}</h1>
      <UButton :label="t('adminPlans.addPlan')" icon="i-lucide-plus" @click="openNewPlanDialog" />
    </div>

    <div v-if="status === 'pending'" class="flex justify-center py-12">
      <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
    </div>
    <UAlert v-else-if="error" color="error" :title="error.message" />
    <template v-else>
      <!-- Desktop table -->
      <div class="hidden md:block">
        <UTable :data="plans ?? []" :columns="columns">
          <template #name-cell="{ row }">
            <code class="bg-[var(--ui-bg-elevated)] px-1.5 py-0.5 rounded text-sm">{{
              row.original.name
            }}</code>
          </template>
          <template #default_speed_limit_mbps-cell="{ row }">
            {{
              row.original.default_speed_limit_mbps
                ? `${row.original.default_speed_limit_mbps} Mbps`
                : t('common.unlimited')
            }}
          </template>
          <template #price_stars-cell="{ row }">
            {{ row.original.price_stars ? `${row.original.price_stars} ⭐` : '-' }}
          </template>
          <template #period_days-cell="{ row }">
            {{
              row.original.period_days
                ? t('adminPlans.days', { n: row.original.period_days })
                : t('adminPlans.permanent')
            }}
          </template>
          <template #trial_minutes-cell="{ row }">
            {{ row.original.trial_minutes ? formatTrial(row.original.trial_minutes) : '-' }}
          </template>
          <template #is_public-cell="{ row }">
            <UBadge
              :color="row.original.is_public ? 'success' : 'neutral'"
              :label="row.original.is_public ? t('common.yes') : t('common.no')"
              variant="subtle"
            />
          </template>
          <template #actions-cell="{ row }">
            <div class="flex gap-1">
              <UButton
                icon="i-lucide-pencil"
                color="neutral"
                variant="ghost"
                size="xs"
                @click="openEditPlanDialog(row.original)"
              />
              <UButton
                icon="i-lucide-trash-2"
                color="error"
                variant="ghost"
                size="xs"
                @click="confirmDeletePlan(row.original)"
              />
            </div>
          </template>
          <template #empty>
            <div class="text-center py-8 text-[var(--ui-text-muted)]">
              {{ t('adminPlans.noPlans') }}
            </div>
          </template>
        </UTable>
      </div>

      <!-- Mobile cards -->
      <div class="md:hidden flex flex-col gap-3">
        <div v-if="!plans?.length" class="text-center py-8 text-[var(--ui-text-muted)]">
          {{ t('adminPlans.noPlans') }}
        </div>
        <UCard v-for="plan in plans ?? []" :key="plan.id">
          <div class="flex justify-between items-start">
            <div>
              <span class="font-medium">{{ plan.display_name }}</span>
              <code
                class="block text-xs bg-[var(--ui-bg-elevated)] px-1 py-0.5 rounded mt-0.5 w-fit"
                >{{ plan.name }}</code
              >
            </div>
            <div class="flex gap-1">
              <UButton
                icon="i-lucide-pencil"
                color="neutral"
                variant="ghost"
                size="xs"
                @click="openEditPlanDialog(plan)"
              />
              <UButton
                icon="i-lucide-trash-2"
                color="error"
                variant="ghost"
                size="xs"
                @click="confirmDeletePlan(plan)"
              />
            </div>
          </div>
          <div class="grid grid-cols-2 gap-x-4 gap-y-1 mt-3 text-sm">
            <span class="text-[var(--ui-text-muted)]">{{ t('adminPlans.speed') }}</span>
            <span>{{
              plan.default_speed_limit_mbps
                ? `${plan.default_speed_limit_mbps} Mbps`
                : t('common.unlimited')
            }}</span>
            <span class="text-[var(--ui-text-muted)]">{{ t('adminPlans.maxPeers') }}</span>
            <span>{{ plan.max_peers }}</span>
            <span class="text-[var(--ui-text-muted)]">{{ t('adminPlans.price') }}</span>
            <span>{{ plan.price_stars ? `${plan.price_stars} ⭐` : '-' }}</span>
            <span class="text-[var(--ui-text-muted)]">{{ t('adminPlans.period') }}</span>
            <span>{{
              plan.period_days
                ? t('adminPlans.days', { n: plan.period_days })
                : t('adminPlans.permanent')
            }}</span>
            <span v-if="plan.trial_minutes" class="text-[var(--ui-text-muted)]">{{
              t('adminPlans.trial')
            }}</span>
            <span v-if="plan.trial_minutes">{{ formatTrial(plan.trial_minutes) }}</span>
          </div>
          <div class="mt-2">
            <UBadge
              :color="plan.is_public ? 'success' : 'neutral'"
              :label="plan.is_public ? t('common.yes') : t('common.no')"
              variant="subtle"
              size="sm"
            />
          </div>
        </UCard>
      </div>
    </template>

    <!-- Plan Dialog -->
    <UModal
      v-model:open="planDialog"
      :title="t(isEditing ? 'adminPlans.editPlan' : 'adminPlans.createPlan')"
    >
      <template #body>
        <div class="flex flex-col gap-4">
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminPlans.internalName') }}</label>
            <UInput
              v-model="planForm.name"
              :disabled="isEditing"
              :placeholder="t('adminPlans.internalNamePlaceholder')"
            />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">{{ t('adminPlans.displayName') }}</label>
            <UInput
              v-model="planForm.display_name"
              :placeholder="t('adminPlans.displayNamePlaceholder')"
            />
          </div>
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">{{ t('adminPlans.speedLimit') }}</label>
              <UInput
                type="number"
                :model-value="planForm.default_speed_limit_mbps?.toString()"
                @update:model-value="
                  (v: string | number) =>
                    (planForm.default_speed_limit_mbps = v === '' ? null : Number(v))
                "
                :placeholder="t('common.unlimited')"
                :min="1"
              />
            </div>
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">{{ t('adminPlans.maxPeers') }}</label>
              <UInput type="number" v-model.number="planForm.max_peers" :min="1" />
            </div>
          </div>
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">{{ t('adminPlans.priceLabel') }}</label>
              <UInput
                type="number"
                :model-value="planForm.price_stars?.toString()"
                @update:model-value="
                  (v: string | number) => (planForm.price_stars = v === '' ? null : Number(v))
                "
                placeholder="-"
                :min="1"
              />
            </div>
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">{{ t('adminPlans.periodDays') }}</label>
              <UInput
                type="number"
                :model-value="planForm.period_days?.toString()"
                @update:model-value="
                  (v: string | number) => (planForm.period_days = v === '' ? null : Number(v))
                "
                :placeholder="t('adminPlans.permanent')"
                :min="1"
              />
            </div>
          </div>
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">{{ t('adminPlans.trialDuration') }}</label>
              <div class="flex gap-2">
                <UInput
                  type="number"
                  class="flex-1"
                  :model-value="trialValue?.toString()"
                  @update:model-value="
                    (v: string | number) => (trialValue = v === '' ? null : Number(v))
                  "
                  :placeholder="t('adminPlans.notATrial')"
                  :min="1"
                />
                <USelect
                  v-model="trialUnit"
                  :items="trialUnitItems"
                  value-key="value"
                  class="w-32"
                />
              </div>
            </div>
          </div>
          <UCheckbox v-model="planForm.is_public" :label="t('adminPlans.publicCheckbox')" />
        </div>
      </template>
      <template #footer>
        <UButton
          :label="t('common.cancel')"
          color="neutral"
          variant="outline"
          @click="planDialog = false"
        />
        <UButton
          :label="isEditing ? t('common.save') : t('common.create')"
          :loading="submitting"
          @click="savePlan"
        />
      </template>
    </UModal>

    <!-- Confirm Delete Dialog -->
    <UModal v-model:open="confirmOpen" :title="t('adminPlans.deletePlan')">
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
          @click="doDeletePlan"
        />
      </template>
    </UModal>
  </div>
</template>
