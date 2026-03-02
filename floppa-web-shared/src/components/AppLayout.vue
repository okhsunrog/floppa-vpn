<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useAuthStore } from '../stores'

const props = withDefaults(defineProps<{
  extraNavItems?: { label: string; icon: string; to: string }[]
}>(), {
  extraNavItems: () => [],
})

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

const isMiniApp = Boolean((window as { Telegram?: { WebApp?: { initData?: string } } }).Telegram?.WebApp?.initData)
const mobileMenuOpen = ref(false)

// Auto-close mobile menu on navigation
watch(() => route.path, () => {
  mobileMenuOpen.value = false
})

function logout() {
  auth.logout()
  router.push('/login')
}

function toggleLocale() {
  locale.value = locale.value === 'ru' ? 'en' : 'ru'
  localStorage.setItem('locale', locale.value)
}

const navItems = computed(() => {
  if (!auth.isAuthenticated) return []

  const items: { label: string; icon: string; to?: string; children?: { label: string; icon: string; to: string }[] }[] = [
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
      { label: t('nav.users'), icon: 'i-lucide-users', to: '/admin/users' },
      { label: t('nav.plans'), icon: 'i-lucide-list', to: '/admin/plans' },
    )
  }

  return items
})
</script>

<template>
  <UApp>
    <header v-if="auth.isAuthenticated" class="sticky z-40 border-b border-[var(--ui-border)] bg-[var(--ui-bg)]" :style="{ top: 'var(--safe-area-inset-top, 0px)' }">
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
        <div class="flex items-center gap-1 sm:gap-2">
          <div class="hidden sm:flex items-center gap-2">
            <UAvatar
              :src="auth.user?.photo_url ?? undefined"
              :alt="navLabel"
              icon="i-lucide-user"
              size="xs"
            />
            <span class="text-sm text-[var(--ui-text-muted)]">{{ navLabel }}</span>
          </div>
          <UColorModeButton />
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
            class="hidden sm:inline-flex"
            @click="logout"
          />
          <UButton
            v-if="!isMiniApp"
            icon="i-lucide-log-out"
            color="neutral"
            variant="ghost"
            size="sm"
            class="sm:hidden"
            @click="logout"
          />
        </div>
      </div>
    </header>

    <!-- Mobile nav slideover -->
    <USlideover v-model:open="mobileMenuOpen" side="left" :close="true" :ui="{ overlay: 'z-50', content: 'z-50 pt-[var(--safe-area-inset-top,0px)]' }">
      <template #header>
        <div class="flex items-center justify-between w-full">
          <span class="font-bold text-lg text-[var(--ui-primary)]">Floppa VPN</span>
          <UButton
            icon="i-lucide-x"
            color="neutral"
            variant="ghost"
            size="sm"
            @click="mobileMenuOpen = false"
          />
        </div>
      </template>
      <template #body>
        <div class="flex flex-col h-full">
          <nav class="flex flex-col gap-1">
            <template v-for="section in (auth.isAdmin ? [
              { items: mobileNavItems.slice(0, 2 + extraNavItems.length) },
              { label: t('nav.admin'), items: mobileNavItems.slice(2 + extraNavItems.length) },
            ] : [
              { items: mobileNavItems },
            ])" :key="section.label ?? 'main'">
              <div v-if="section.label" class="px-3 pt-4 pb-1 text-xs font-semibold uppercase tracking-wider text-[var(--ui-text-muted)]">
                {{ section.label }}
              </div>
              <RouterLink
                v-for="item in section.items"
                :key="item.to"
                :to="item.to"
                class="flex items-center gap-3 px-3 py-2.5 rounded-lg text-sm font-medium transition-colors"
                :class="route.path === item.to
                  ? 'bg-[var(--ui-bg-elevated)] text-[var(--ui-primary)]'
                  : 'text-[var(--ui-text)] hover:bg-[var(--ui-bg-elevated)]'"
              >
                <UIcon :name="item.icon" class="size-5" />
                {{ item.label }}
              </RouterLink>
            </template>
          </nav>

          <div class="mt-auto pt-4 border-t border-[var(--ui-border)]">
            <div class="flex items-center gap-2 px-3 py-2 text-sm text-[var(--ui-text-muted)]">
              <UAvatar
                :src="auth.user?.photo_url ?? undefined"
                :alt="navLabel"
                icon="i-lucide-user"
                size="2xs"
              />
              {{ navLabel }}
            </div>
            <div class="flex items-center gap-2 px-3 py-2">
              <UColorModeButton />
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
        </div>
      </template>
    </USlideover>

    <main class="max-w-7xl mx-auto p-4 md:p-6">
      <slot />
    </main>
  </UApp>
</template>
