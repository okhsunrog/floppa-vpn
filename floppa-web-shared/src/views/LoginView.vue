<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getPublicConfigQuery } from '../client/@pinia/colada.gen'
import { telegramLogin, telegramMiniAppAuth } from '../client/sdk.gen'
import type { TelegramAuthData } from '../client/types.gen'
import { useAuthStore } from '../stores'
import TelegramLoginButton from '../components/TelegramLoginButton.vue'

const props = withDefaults(
  defineProps<{
    authMode?: 'widget' | 'deep-link'
    deepLinkLoginUrl?: string
  }>(),
  {
    authMode: 'widget',
  },
)

const emit = defineEmits<{
  'deep-link-login': [url: string]
}>()

const auth = useAuthStore()
const router = useRouter()
const { t } = useI18n()
const { data: config, status, error: configError } = useQuery(getPublicConfigQuery())
const loginError = ref<string | null>(null)
const miniAppLoading = ref(false)

// Detect Telegram Mini App environment
function getTelegramInitData(): string | null {
  try {
    const tg = (window as { Telegram?: { WebApp?: { initData?: string } } }).Telegram
    const initData = tg?.WebApp?.initData
    return initData && initData.length > 0 ? initData : null
  } catch {
    return null
  }
}

onMounted(async () => {
  const initData = getTelegramInitData()
  if (!initData) return

  // Signal to Telegram that the app is ready
  try {
    const tg = (window as { Telegram?: { WebApp?: { ready?: () => void; expand?: () => void } } })
      .Telegram
    tg?.WebApp?.ready?.()
    tg?.WebApp?.expand?.()
  } catch {
    /* ignore */
  }

  // Auto-login with Mini App initData
  miniAppLoading.value = true
  try {
    const { data: response } = await telegramMiniAppAuth({
      body: { init_data: initData },
      throwOnError: true,
    })
    auth.setAuth(response.token, response.user)
    router.push('/')
  } catch {
    loginError.value = t('login.miniAppFailed')
  } finally {
    miniAppLoading.value = false
  }
})

async function handleTelegramAuth(data: TelegramAuthData) {
  loginError.value = null
  try {
    const { data: response } = await telegramLogin({ body: data, throwOnError: true })
    auth.setAuth(response.token, response.user)
    router.push('/')
  } catch (e) {
    loginError.value = e instanceof Error ? e.message : t('login.loginFailed')
  }
}

function startDeepLinkLogin() {
  if (props.deepLinkLoginUrl) {
    emit('deep-link-login', props.deepLinkLoginUrl)
  }
}
</script>

<template>
  <div class="flex items-center justify-center min-h-screen p-4">
    <UCard class="w-full max-w-sm">
      <template #header>
        <div class="flex items-center justify-center gap-2 text-2xl font-bold">
          <UIcon name="i-lucide-shield" class="text-[var(--ui-primary)]" />
          <span>Floppa VPN</span>
        </div>
        <p class="text-center text-sm text-[var(--ui-text-muted)] mt-1">
          {{ t('login.subtitle') }}
        </p>
      </template>

      <div class="flex flex-col items-center gap-4 py-4">
        <!-- Mini App auto-login in progress -->
        <template v-if="miniAppLoading">
          <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
          <p class="text-sm text-[var(--ui-text-muted)]">{{ t('login.miniAppLoggingIn') }}</p>
        </template>

        <template v-else>
          <div
            v-if="status === 'pending'"
            class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]"
          />
          <UAlert v-else-if="configError" color="error" :title="t('login.configError')" />
          <UAlert v-else-if="loginError" color="error" :title="loginError" />
          <UAlert
            v-else-if="!config?.telegram_bot_username"
            color="warning"
            :title="t('login.botNotConfigured')"
          />

          <!-- Deep-link mode (Tauri desktop/mobile) -->
          <div
            v-else-if="authMode === 'deep-link' && deepLinkLoginUrl"
            class="flex flex-col items-center gap-3 w-full"
          >
            <UButton color="primary" icon="i-lucide-send" @click="startDeepLinkLogin">
              {{ t('login.continueInBrowser') }}
            </UButton>
            <div class="flex flex-col gap-1.5 mt-1">
              <p class="text-xs text-[var(--ui-text-muted)] text-center max-w-xs">
                {{ t('login.browserHint') }}
              </p>
              <p class="text-xs text-[var(--ui-text-muted)] text-center max-w-xs">
                {{ t('login.retryHint') }}
              </p>
              <p class="text-xs text-[var(--ui-text-muted)] text-center max-w-xs">
                {{ t('login.browserChangeHint') }}
              </p>
              <p class="text-xs text-[var(--ui-text-muted)] text-center max-w-xs">
                {{ t('login.vpnHint') }}
              </p>
            </div>
          </div>

          <!-- Widget mode (web) -->
          <TelegramLoginButton
            v-else
            :bot-name="config.telegram_bot_username"
            @auth="handleTelegramAuth"
          />
        </template>
      </div>
    </UCard>
  </div>
</template>
