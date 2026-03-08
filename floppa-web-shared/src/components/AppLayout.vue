<script setup lang="ts">
import { computed, ref, watch, onMounted, onUnmounted, nextTick } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import type { RouteLocationNormalized } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useColorMode } from '@vueuse/core'
import { useAuthStore } from '../stores'
import ColorModeButton from './ColorModeButton.vue'

const props = withDefaults(
  defineProps<{
    extraNavItems?: { label: string; icon: string; to: string }[]
  }>(),
  {
    extraNavItems: () => [],
  },
)

const auth = useAuthStore()
const router = useRouter()
const route = useRoute()
const { t, locale } = useI18n()

const navLabel = computed(() => {
  const u = auth.user
  if (!u) return ''
  if (u.username) return `@${u.username}`
  if (u.first_name) return u.last_name ? `${u.first_name} ${u.last_name}` : u.first_name
  return `User #${u.id}`
})

const isMiniApp = Boolean(
  (window as { Telegram?: { WebApp?: { initData?: string } } }).Telegram?.WebApp?.initData,
)
const mobileMenuOpen = ref(false)

// Android back button support for sidebar via history API.
//
// Open: push history state so back button has an entry to pop.
// Back button: popstate fires → close sidebar (entry already consumed).
// Manual close (X/overlay): history.back() to remove the entry.
// Navigation: beforeEach cancels nav, history.back() removes entry,
//   then popstate re-navigates to the original destination.
let closedByBackButton = false
let pendingNavigation: RouteLocationNormalized | null = null

watch(mobileMenuOpen, (open) => {
  if (open) {
    history.pushState({ mobileMenu: true }, '')
  } else if (!closedByBackButton) {
    history.back()
  }
  closedByBackButton = false
})

function onPopState() {
  if (mobileMenuOpen.value) {
    closedByBackButton = true
    mobileMenuOpen.value = false
  }
  if (pendingNavigation) {
    const dest = pendingNavigation
    pendingNavigation = null
    nextTick(() => router.push(dest))
  }
}

onMounted(() => window.addEventListener('popstate', onPopState))
onUnmounted(() => {
  window.removeEventListener('popstate', onPopState)
  removeBeforeEach()
})

// Intercept navigation while sidebar is open: close sidebar first,
// then re-navigate after the stale history entry is removed.
const removeBeforeEach = router.beforeEach((to) => {
  if (mobileMenuOpen.value) {
    pendingNavigation = to
    closedByBackButton = true
    mobileMenuOpen.value = false
    history.back()
    return false
  }
})

function logout() {
  auth.logout()
  router.push('/login')
}

const { store: colorModeStore } = useColorMode()

function setLocale(lang: string) {
  locale.value = lang
  localStorage.setItem('locale', lang)
}

function toggleLocale() {
  setLocale(locale.value === 'ru' ? 'en' : 'ru')
}

const colorModes = [
  { value: 'light', icon: 'i-lucide-sun' },
  { value: 'dark', icon: 'i-lucide-moon' },
  { value: 'auto', icon: 'i-lucide-monitor' },
] as const

const locales = [
  { value: 'en', label: 'English' },
  { value: 'ru', label: 'Русский' },
] as const

const navItems = computed(() => {
  if (!auth.isAuthenticated) return []

  const items: {
    label: string
    icon: string
    to?: string
    children?: { label: string; icon: string; to: string }[]
  }[] = [
    { label: t('nav.dashboard'), icon: 'i-lucide-home', to: '/' },
    { label: t('nav.myConfigs'), icon: 'i-lucide-key', to: '/peers' },
  ]

  for (const item of props.extraNavItems) {
    items.push(item)
  }

  if (auth.isAdmin) {
    items.push({
      label: t('nav.admin'),
      icon: 'i-lucide-settings',
      children: [
        { label: t('nav.overview'), icon: 'i-lucide-bar-chart-3', to: '/admin' },
        { label: t('nav.peers'), icon: 'i-lucide-link', to: '/admin/peers' },
        { label: t('nav.vless'), icon: 'i-lucide-globe', to: '/admin/vless' },
        {
          label: t('nav.installations'),
          icon: 'i-lucide-monitor-smartphone',
          to: '/admin/installations',
        },
        { label: t('nav.users'), icon: 'i-lucide-users', to: '/admin/users' },
        { label: t('nav.plans'), icon: 'i-lucide-list', to: '/admin/plans' },
      ],
    })
  }

  return items
})

// Flat nav items for mobile (no nested children)
const mobileNavItems = computed(() => {
  if (!auth.isAuthenticated) return []

  const items: { label: string; icon: string; to: string }[] = [
    { label: t('nav.dashboard'), icon: 'i-lucide-home', to: '/' },
    { label: t('nav.myConfigs'), icon: 'i-lucide-key', to: '/peers' },
  ]

  for (const item of props.extraNavItems) {
    items.push(item)
  }

  if (auth.isAdmin) {
    items.push(
      { label: t('nav.overview'), icon: 'i-lucide-bar-chart-3', to: '/admin' },
      { label: t('nav.peers'), icon: 'i-lucide-link', to: '/admin/peers' },
      { label: t('nav.vless'), icon: 'i-lucide-globe', to: '/admin/vless' },
      {
        label: t('nav.installations'),
        icon: 'i-lucide-monitor-smartphone',
        to: '/admin/installations',
      },
      { label: t('nav.users'), icon: 'i-lucide-users', to: '/admin/users' },
      { label: t('nav.plans'), icon: 'i-lucide-list', to: '/admin/plans' },
    )
  }

  return items
})
</script>

<template>
  <UApp>
    <header
      v-if="auth.isAuthenticated"
      class="sticky z-40 border-b border-[var(--ui-border)] bg-[var(--ui-bg)]"
      :style="{ top: 'var(--safe-area-inset-top, 0px)' }"
    >
      <div class="max-w-7xl mx-auto px-4 h-14 flex items-center justify-between">
        <div class="flex items-center gap-2 md:gap-4">
          <!-- Mobile hamburger -->
          <UButton
            class="md:hidden"
            icon="i-lucide-menu"
            color="neutral"
            variant="ghost"
            size="sm"
            @click="mobileMenuOpen = true"
          />
          <span class="font-bold text-lg text-[var(--ui-primary)]">Floppa VPN</span>
          <!-- Desktop nav -->
          <nav class="hidden md:flex">
            <UNavigationMenu :items="navItems" />
          </nav>
        </div>
        <div class="hidden md:flex items-center gap-2">
          <UAvatar :src="auth.avatarUrl" :alt="navLabel" icon="i-lucide-user" size="xs" />
          <span class="text-sm text-[var(--ui-text-muted)]">{{ navLabel }}</span>
          <ColorModeButton />
          <UButton
            :label="locale === 'ru' ? 'EN' : 'RU'"
            color="neutral"
            variant="ghost"
            size="sm"
            @click="toggleLocale"
          />
          <UButton
            v-if="!isMiniApp"
            icon="i-lucide-log-out"
            :label="t('nav.logout')"
            color="neutral"
            variant="ghost"
            size="sm"
            @click="logout"
          />
        </div>
      </div>
    </header>

    <!-- Mobile nav slideover -->
    <USlideover
      v-model:open="mobileMenuOpen"
      side="left"
      :close="true"
      :description="t('nav.navigation')"
      :ui="{
        overlay: 'z-50',
        content: 'z-50 pt-[var(--safe-area-inset-top,0px)] pb-[var(--safe-area-inset-bottom,0px)]',
        close: 'top-[calc(1rem+var(--safe-area-inset-top,0px))]',
      }"
    >
      <template #title>
        <span class="font-bold text-lg text-[var(--ui-primary)]">Floppa VPN</span>
      </template>
      <template #body>
        <div class="flex flex-col h-full">
          <nav class="flex flex-col gap-1">
            <template
              v-for="section in auth.isAdmin
                ? [
                    { items: mobileNavItems.slice(0, 2 + extraNavItems.length) },
                    {
                      label: t('nav.admin'),
                      items: mobileNavItems.slice(2 + extraNavItems.length),
                    },
                  ]
                : [{ items: mobileNavItems }]"
              :key="section.label ?? 'main'"
            >
              <div
                v-if="section.label"
                class="px-3 pt-4 pb-1 text-xs font-semibold uppercase tracking-wider text-[var(--ui-text-muted)]"
              >
                {{ section.label }}
              </div>
              <RouterLink
                v-for="item in section.items"
                :key="item.to"
                :to="item.to"
                class="flex items-center gap-3 px-3 py-2.5 rounded-lg text-sm font-medium transition-colors"
                :class="
                  route.path === item.to
                    ? 'bg-[var(--ui-bg-elevated)] text-[var(--ui-primary)]'
                    : 'text-[var(--ui-text)] hover:bg-[var(--ui-bg-elevated)]'
                "
              >
                <UIcon :name="item.icon" class="size-5" />
                {{ item.label }}
              </RouterLink>
            </template>
          </nav>

          <div
            class="mt-auto pt-4 border-t border-[var(--ui-border)] flex flex-col gap-3 px-3 pb-2"
          >
            <UFieldGroup>
              <UButton
                v-for="mode in colorModes"
                :key="mode.value"
                :icon="mode.icon"
                :label="t(`nav.theme.${mode.value}`)"
                :color="colorModeStore === mode.value ? 'primary' : 'neutral'"
                :variant="colorModeStore === mode.value ? 'subtle' : 'ghost'"
                size="sm"
                @click="colorModeStore = mode.value"
              />
            </UFieldGroup>
            <UFieldGroup>
              <UButton
                v-for="lang in locales"
                :key="lang.value"
                :label="lang.label"
                :color="locale === lang.value ? 'primary' : 'neutral'"
                :variant="locale === lang.value ? 'subtle' : 'ghost'"
                size="sm"
                @click="setLocale(lang.value)"
              />
            </UFieldGroup>
            <div class="flex items-center gap-2">
              <UAvatar :src="auth.avatarUrl" :alt="navLabel" icon="i-lucide-user" size="2xs" />
              <span class="text-sm text-[var(--ui-text-muted)]">{{ navLabel }}</span>
              <UButton
                v-if="!isMiniApp"
                icon="i-lucide-log-out"
                :label="t('nav.logout')"
                color="neutral"
                variant="ghost"
                size="sm"
                class="ml-auto"
                @click="logout"
              />
            </div>
          </div>
        </div>
      </template>
    </USlideover>

    <main class="max-w-7xl mx-auto p-4 md:p-6">
      <slot />
    </main>
  </UApp>
</template>
