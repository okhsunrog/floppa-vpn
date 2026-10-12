<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ADMIN_NAVIGATION, useAdminNavigation } from '../composables/adminNavigation'
const route = useRoute()
const { t } = useI18n()
const items = useAdminNavigation()
const active = computed(
  () =>
    items.value.find((item) => item.to !== '/admin' && route.path.startsWith(item.to))?.to ??
    '/admin',
)
</script>

<template>
  <div
    :class="
      ADMIN_NAVIGATION === 'sidebar' ? 'xl:grid xl:grid-cols-[12rem_minmax(0,1fr)] xl:gap-8' : ''
    "
  >
    <template v-if="ADMIN_NAVIGATION !== 'overview'">
      <nav
        :aria-label="t('adminWorkspace.sections')"
        :class="
          ADMIN_NAVIGATION === 'sidebar'
            ? 'hidden xl:flex xl:flex-col self-start sticky top-20 gap-1 border-r border-[var(--ui-border)] pr-4'
            : 'hidden xl:flex flex-wrap gap-1 mb-7 border-b border-[var(--ui-border)] pb-2'
        "
      >
        <RouterLink
          v-for="item in items"
          :key="item.to"
          :to="item.to"
          :aria-current="active === item.to ? 'page' : undefined"
          class="flex items-center gap-2 rounded-lg px-3 py-3 text-sm font-medium transition-colors focus-visible:outline-2 focus-visible:outline-[var(--ui-primary)]"
          :class="
            active === item.to
              ? 'bg-[var(--ui-bg-elevated)] text-[var(--ui-text-highlighted)]'
              : 'text-[var(--ui-text-muted)] hover:bg-[var(--ui-bg-elevated)] hover:text-[var(--ui-text)]'
          "
        >
          <UIcon :name="item.icon" class="size-4 shrink-0" />{{ item.label }}
        </RouterLink>
      </nav>
    </template>
    <div class="min-w-0"><slot /></div>
  </div>
</template>
