<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getPublicConfigQuery } from '../client/@pinia/colada.gen'
import {
  telegramLogin,
  telegramMiniAppAuth,
  exchangeTelegramLoginCode,
  registerAccount,
  loginAccount,
} from '../client/sdk.gen'
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
const manualCode = ref('')
const manualCodeLoading = ref(false)
const manualCodeError = ref<string | null>(null)

// Login surface: account (login + password) leads; Telegram is secondary.
const tab = ref<'account' | 'telegram'>('account')
const accountMode = ref<'register' | 'login'>('register')
const accountLogin = ref('')
const accountPassword = ref('')
const accountLoading = ref(false)
const accountError = ref<string | null>(null)

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
    // Extract Telegram user ID for account-switch detection
    let telegramUserId: number | undefined
    try {
      const userJson = new URLSearchParams(initData).get('user')
      if (userJson) {
        const id = JSON.parse(userJson).id
        if (typeof id === 'number') telegramUserId = id
      }
    } catch {
      /* non-critical */
    }

    const { data: response } = await telegramMiniAppAuth({
      body: { init_data: initData },
      throwOnError: true,
    })
    auth.setAuth(response.token, response.user, telegramUserId)
    router.push('/')
  } catch {
    loginError.value = t('login.miniAppFailed')
  } finally {
    miniAppLoading.value = false
  }
})

async function handleAccount() {
  const login = accountLogin.value.trim()
  if (!login || !accountPassword.value) return
  accountError.value = null
  accountLoading.value = true
  try {
    const call = accountMode.value === 'register' ? registerAccount : loginAccount
    const { data: response } = await call({
      body: { login, password: accountPassword.value },
      throwOnError: true,
    })
    auth.setAuth(response.token, response.user)
    router.push('/')
  } catch (e) {
    const status = (e as { status?: number })?.status
    if (accountMode.value === 'register' && status === 409) {
      accountError.value = t('login.loginTaken')
    } else if (accountMode.value === 'register') {
      accountError.value = t('login.registerFailed')
    } else {
      accountError.value = t('login.accountLoginFailed')
    }
  } finally {
    accountLoading.value = false
  }
}

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

async function handleManualCode() {
  const code = manualCode.value.trim()
  if (!code) return
  manualCodeError.value = null
  manualCodeLoading.value = true
  try {
    const { data: response } = await exchangeTelegramLoginCode({
      body: { code },
      throwOnError: true,
    })
    auth.setAuth(response.token, response.user)
    router.push('/')
  } catch {
    manualCodeError.value = t('login.codeInvalid')
  } finally {
    manualCodeLoading.value = false
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

      <div class="flex flex-col gap-4 py-4">
        <!-- Mini App auto-login in progress (takes precedence over tabs) -->
        <template v-if="miniAppLoading">
          <div class="flex flex-col items-center gap-4">
            <div class="animate-spin i-lucide-loader-2 size-8 text-[var(--ui-primary)]" />
            <p class="text-sm text-[var(--ui-text-muted)]">{{ t('login.miniAppLoggingIn') }}</p>
          </div>
        </template>

        <template v-else>
          <!-- Tab toggle: Account (primary) / Telegram (secondary) -->
          <div class="flex gap-1 p-1 rounded-lg bg-[var(--ui-bg-elevated)]">
            <UButton
              :variant="tab === 'account' ? 'solid' : 'ghost'"
              :color="tab === 'account' ? 'primary' : 'neutral'"
              block
              class="flex-1"
              @click="tab = 'account'"
            >
              {{ t('login.tabAccount') }}
            </UButton>
            <UButton
              :variant="tab === 'telegram' ? 'solid' : 'ghost'"
              :color="tab === 'telegram' ? 'primary' : 'neutral'"
              block
              class="flex-1"
              @click="tab = 'telegram'"
            >
              Telegram
            </UButton>
          </div>

          <!-- ACCOUNT tab: login + password -->
          <div v-show="tab === 'account'" class="flex flex-col gap-3">
            <div class="flex text-sm rounded-md overflow-hidden border border-[var(--ui-border)]">
              <button
                type="button"
                class="flex-1 py-1.5"
                :class="
                  accountMode === 'register'
                    ? 'bg-[var(--ui-bg-elevated)] font-medium'
                    : 'text-[var(--ui-text-muted)]'
                "
                @click="accountMode = 'register'"
              >
                {{ t('login.createAccount') }}
              </button>
              <button
                type="button"
                class="flex-1 py-1.5"
                :class="
                  accountMode === 'login'
                    ? 'bg-[var(--ui-bg-elevated)] font-medium'
                    : 'text-[var(--ui-text-muted)]'
                "
                @click="accountMode = 'login'"
              >
                {{ t('login.signIn') }}
              </button>
            </div>

            <UInput
              v-model="accountLogin"
              :placeholder="t('login.loginPlaceholder')"
              autocomplete="username"
              icon="i-lucide-user"
            />
            <UInput
              v-model="accountPassword"
              type="password"
              :placeholder="t('login.passwordPlaceholder')"
              autocomplete="current-password"
              icon="i-lucide-lock"
              @keydown.enter="handleAccount"
            />
            <UButton
              block
              color="primary"
              :loading="accountLoading"
              :disabled="!accountLogin.trim() || !accountPassword"
              @click="handleAccount"
            >
              {{ accountMode === 'register' ? t('login.createAccount') : t('login.signIn') }}
            </UButton>
            <UAlert v-if="accountError" color="error" :title="accountError" />
            <p
              v-if="accountMode === 'register'"
              class="text-xs text-[var(--ui-text-muted)] text-center"
            >
              {{ t('login.registerHint') }}
            </p>
          </div>

          <!-- TELEGRAM tab: existing widget / deep-link flow -->
          <div v-show="tab === 'telegram'" class="flex flex-col items-center gap-4">
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
              <p class="text-xs text-[var(--ui-text-muted)] text-center max-w-xs mt-1">
                {{ t('login.browserHint') }}
              </p>
              <p class="text-xs text-[var(--ui-text-muted)] text-center max-w-xs">
                {{ t('login.vpnHint') }}
              </p>

              <!-- Manual code fallback -->
              <div class="w-full border-t border-[var(--ui-border)] pt-3 mt-2">
                <p class="text-xs text-[var(--ui-text-muted)] text-center mb-2">
                  {{ t('login.codeFallbackHint') }}
                </p>
                <div class="flex gap-2">
                  <UInput
                    v-model="manualCode"
                    :placeholder="t('login.codePlaceholder')"
                    class="flex-1"
                    @keydown.enter="handleManualCode"
                  />
                  <UButton
                    :loading="manualCodeLoading"
                    :disabled="!manualCode.trim()"
                    @click="handleManualCode"
                  >
                    {{ t('login.codeSubmit') }}
                  </UButton>
                </div>
                <UAlert
                  v-if="manualCodeError"
                  color="error"
                  :title="manualCodeError"
                  class="mt-2"
                />
              </div>
            </div>

            <!-- Widget mode (web) -->
            <TelegramLoginButton
              v-else-if="config?.telegram_bot_username"
              :bot-name="config.telegram_bot_username"
              @auth="handleTelegramAuth"
            />
          </div>
        </template>
      </div>
    </UCard>
  </div>
</template>
