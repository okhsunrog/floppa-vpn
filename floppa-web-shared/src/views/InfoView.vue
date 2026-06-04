<script setup lang="ts">
import { computed, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { listPublicPlansQuery, getPublicConfigQuery } from '../client/@pinia/colada.gen'
import { client } from '../client/client.gen'
import { useAuthStore } from '../stores'
import changelogData from '../changelog.json'
import logoUrl from '../assets/logo.png'
import ColorModeButton from '../components/ColorModeButton.vue'
import { durationUnit } from '../utils/format'

// `landing` is shown to logged-out visitors (web home, with a Login CTA);
// `tab` is the in-app Info tab for authenticated users.
const props = withDefaults(
  defineProps<{
    variant?: 'landing' | 'tab'
    // Injected by the Tauri client to open links in the system browser. On web, falls back
    // to window.open (which also triggers binary downloads).
    openExternal?: (url: string) => void
  }>(),
  { variant: 'tab' },
)

const { t, locale } = useI18n()
const router = useRouter()
const auth = useAuthStore()

// Trial duration is stored in minutes; render it in the largest whole unit.
function formatTrialBadge(minutes: number): string {
  const { unit, n } = durationUnit(minutes)
  return t('info.trialBadge', { d: t(`info.${unit}`, { n }) })
}

const GITHUB_URL = 'https://github.com/okhsunrog/floppa-vpn'
const CHANNEL_URL = 'https://t.me/floppa_vpn_channel'

const { data: plans } = useQuery(listPublicPlansQuery())
const { data: publicConfig } = useQuery(getPublicConfigQuery())

const botUrl = computed(() => {
  const username = publicConfig.value?.telegram_bot_username
  return username ? `https://t.me/${username}` : null
})

// Release binaries are mirrored on our own server at <origin>/downloads/. Derive the base from
// the configured API base ('/api' on web → same-origin '/downloads'; absolute on the Tauri client).
const downloadsBase = computed(() => {
  const apiBase = client.getConfig().baseUrl ?? '/api'
  return apiBase.replace(/\/api\/?$/, '').replace(/\/+$/, '') + '/downloads'
})

const downloads = computed(() => [
  { key: 'android', icon: 'i-lucide-smartphone', file: 'floppa-vpn-android-arm64.apk' },
  { key: 'windows', icon: 'i-lucide-monitor', file: 'floppa-vpn-windows-x86_64.exe' },
  { key: 'linux', icon: 'i-lucide-laptop', file: 'floppa-vpn-linux-x86_64.AppImage' },
])

const secondaryDownloads = computed(() => [
  { key: 'deb', file: 'floppa-vpn-linux-x86_64.deb' },
  { key: 'arch', file: 'floppa-vpn-x86_64.pkg.tar.zst' },
])

function go(url: string) {
  if (props.openExternal) props.openExternal(url)
  else window.open(url, '_blank', 'noopener,noreferrer')
}

function download(file: string) {
  go(`${downloadsBase.value}/${file}`)
}

function planPrice(price: number | null | undefined): string {
  return price ? `${price} ⭐` : t('common.free')
}

function toggleLocale() {
  const next = locale.value === 'ru' ? 'en' : 'ru'
  locale.value = next
  localStorage.setItem('locale', next)
}

// Changelog (bundled at build time from floppa-web-shared/changelog.json; shown on web + client).
interface ChangelogItem {
  en: string
  ru: string
}
interface ChangelogSection {
  type: string
  items: ChangelogItem[]
}
interface ChangelogEntry {
  version: string
  sections: ChangelogSection[]
}

const changelogHistory = (changelogData.history ?? []) as ChangelogEntry[]
const showHistory = ref(false)
const visibleChangelog = computed<ChangelogEntry[]>(() =>
  showHistory.value ? [changelogData, ...changelogHistory] : [changelogData],
)

const sectionIcons: Record<string, string> = {
  added: 'i-lucide-plus-circle',
  fixed: 'i-lucide-wrench',
  changed: 'i-lucide-refresh-cw',
  notes: 'i-lucide-info',
}

function changelogText(item: ChangelogItem): string {
  return locale.value === 'ru' ? item.ru : item.en
}

/** Render markdown-style [text](url) links as anchors (clicks intercepted by onChangelogClick). */
function renderLinks(text: string): string {
  return text.replace(
    /\[([^\]]+)\]\(([^)]+)\)/g,
    '<a href="$2" class="underline text-[var(--ui-primary)] hover:opacity-80">$1</a>',
  )
}

function onChangelogClick(event: MouseEvent) {
  const target = (event.target as HTMLElement).closest('a')
  if (target?.href) {
    event.preventDefault()
    go(target.href)
  }
}
</script>

<template>
  <div class="max-w-5xl mx-auto">
    <!-- Top bar only for logged-out visitors; authenticated users already have the app navbar. -->
    <div
      v-if="variant === 'landing' && !auth.isAuthenticated"
      class="flex items-center justify-between mb-8"
    >
      <span class="font-bold text-xl text-[var(--ui-primary)]">Floppa VPN</span>
      <div class="flex items-center gap-2">
        <ColorModeButton />
        <UButton
          :label="locale === 'ru' ? 'EN' : 'RU'"
          color="neutral"
          variant="ghost"
          size="sm"
          @click="toggleLocale"
        />
        <UButton :label="t('info.login')" icon="i-lucide-log-in" @click="router.push('/login')" />
      </div>
    </div>

    <!-- Hero -->
    <section class="text-center mb-10">
      <img
        v-if="variant === 'landing'"
        :src="logoUrl"
        alt="Floppa VPN"
        class="w-32 h-32 md:w-40 md:h-40 mx-auto mb-5 drop-shadow"
      />
      <h1 class="text-3xl md:text-4xl font-bold mb-3">{{ t('info.heroTitle') }}</h1>
      <p class="text-[var(--ui-text-muted)] max-w-2xl mx-auto">{{ t('info.heroSubtitle') }}</p>
      <div class="flex flex-wrap justify-center gap-2 mt-4">
        <UBadge color="primary" variant="subtle" :label="t('vpn.amneziawg')" />
        <UBadge color="neutral" variant="subtle" :label="t('vpn.wireguard')" />
        <UBadge color="neutral" variant="subtle" :label="t('vpn.vless')" />
      </div>
    </section>

    <!-- Installed app version (client injects this via the slot; empty on web) -->
    <div v-if="$slots['app-info']" class="text-center -mt-6 mb-10">
      <slot name="app-info" />
    </div>

    <!-- Plans -->
    <section v-if="plans?.length" class="mb-10">
      <h2 class="text-xl font-semibold mb-4 text-center">{{ t('info.plansTitle') }}</h2>
      <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        <UCard v-for="plan in plans" :key="plan.id">
          <div class="flex items-baseline justify-between mb-2">
            <span class="font-semibold text-lg">{{ plan.display_name }}</span>
            <UBadge
              v-if="plan.trial_minutes"
              color="success"
              variant="subtle"
              :label="formatTrialBadge(plan.trial_minutes)"
            />
          </div>
          <div class="text-2xl font-bold mb-3">
            {{ planPrice(plan.price_stars) }}
            <span v-if="plan.period_days" class="text-sm font-normal text-[var(--ui-text-muted)]">
              / {{ t('info.days', { n: plan.period_days }) }}
            </span>
          </div>
          <ul class="text-sm flex flex-col gap-1.5 text-[var(--ui-text-muted)]">
            <li class="flex items-center gap-2">
              <UIcon name="i-lucide-gauge" class="size-4" />
              {{
                plan.default_speed_limit_mbps
                  ? `${plan.default_speed_limit_mbps} Mbps`
                  : t('common.unlimited')
              }}
            </li>
            <li class="flex items-center gap-2">
              <UIcon name="i-lucide-monitor-smartphone" class="size-4" />
              {{ t('info.devices', { n: plan.max_peers }) }}
            </li>
          </ul>
        </UCard>
      </div>
      <div v-if="botUrl" class="flex justify-center mt-5">
        <UButton :label="t('info.buyViaBot')" icon="i-lucide-send" size="lg" @click="go(botUrl)" />
      </div>
    </section>

    <!-- Downloads -->
    <section class="mb-10">
      <h2 class="text-xl font-semibold mb-4 text-center">{{ t('info.downloadsTitle') }}</h2>
      <div class="flex flex-wrap justify-center gap-3">
        <UButton
          v-for="d in downloads"
          :key="d.key"
          :icon="d.icon"
          :label="t(`info.download_${d.key}`)"
          color="neutral"
          variant="subtle"
          size="lg"
          @click="download(d.file)"
        />
      </div>
      <div class="flex flex-wrap justify-center gap-x-4 gap-y-1 mt-3 text-sm">
        <button
          v-for="d in secondaryDownloads"
          :key="d.key"
          class="text-[var(--ui-text-muted)] hover:text-[var(--ui-primary)] underline underline-offset-2"
          @click="download(d.file)"
        >
          {{ t(`info.download_${d.key}`) }}
        </button>
      </div>
    </section>

    <!-- What's new (changelog, bundled) -->
    <section v-if="changelogData.sections?.length" class="mb-10">
      <h2 class="text-xl font-semibold mb-4 text-center">{{ t('info.changelogTitle') }}</h2>
      <div class="max-w-2xl mx-auto flex flex-col gap-6" @click="onChangelogClick">
        <div v-for="entry in visibleChangelog" :key="entry.version">
          <h3 class="font-semibold mb-2">v{{ entry.version }}</h3>
          <div
            v-for="section in entry.sections.filter((s) => s.items.length)"
            :key="section.type"
            class="mb-3"
          >
            <div
              class="flex items-center gap-1.5 text-sm font-medium text-[var(--ui-text-muted)] mb-1"
            >
              <UIcon :name="sectionIcons[section.type] ?? 'i-lucide-dot'" class="size-4" />
              {{ t(`changelog.${section.type}`) }}
            </div>
            <ul class="flex flex-col gap-1">
              <!-- eslint-disable-next-line vue/no-v-html -->
              <li
                v-for="(item, i) in section.items"
                :key="i"
                class="text-sm rounded-lg bg-[var(--ui-bg-elevated)] px-3 py-2"
                v-html="renderLinks(changelogText(item))"
              />
            </ul>
          </div>
        </div>
      </div>
      <div v-if="changelogHistory.length" class="flex justify-center mt-2">
        <UButton
          :label="showHistory ? t('common.close') : t('info.olderVersions')"
          color="neutral"
          variant="ghost"
          size="sm"
          @click="showHistory = !showHistory"
        />
      </div>
    </section>

    <!-- Links -->
    <section class="flex flex-wrap justify-center gap-3">
      <UButton
        :label="t('info.channel')"
        icon="i-lucide-megaphone"
        color="neutral"
        variant="ghost"
        @click="go(CHANNEL_URL)"
      />
      <UButton
        v-if="botUrl"
        :label="t('info.bot')"
        icon="i-lucide-bot"
        color="neutral"
        variant="ghost"
        @click="go(botUrl)"
      />
      <UButton
        :label="t('info.github')"
        icon="i-lucide-github"
        color="neutral"
        variant="ghost"
        @click="go(GITHUB_URL)"
      />
    </section>
  </div>
</template>
