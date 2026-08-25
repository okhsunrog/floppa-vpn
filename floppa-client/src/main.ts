import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import piniaPluginPersistedstate from 'pinia-plugin-persistedstate'
import { PiniaColada } from '@pinia/colada'
import { createRouter, createWebHistory } from 'vue-router'
import ui from '@nuxt/ui/vue-plugin'
import { isTauri } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrent, onOpenUrl } from '@tauri-apps/plugin-deep-link'
import { createSharedI18n, installApiInterceptors, useAuthStore } from 'floppa-web-shared'
import { useUpdateStore } from './stores/updateStore'
import { useDeepLinkAuthStore } from './stores/deepLinkAuthStore'
import { commands } from './bindings'
import { client } from 'floppa-web-shared/client/client.gen'
import { exchangeTelegramLoginCode } from 'floppa-web-shared/client/sdk.gen'
import { createAppRoutes, installAuthGuard } from 'floppa-web-shared/router'
import { trace, debug, info, warn, error } from '@tauri-apps/plugin-log'

import './styles.css'
import App from './App.vue'
import ClientLoginView from './views/ClientLoginView.vue'
import ClientDashboardView from './views/ClientDashboardView.vue'
import ClientInfoView from './views/ClientInfoView.vue'
import { useVpnStore } from './stores/vpnStore'
import { extractDeepLinkLoginCode } from './utils/deepLink'
import { API_URL } from './config'

// Forward console.* to Tauri's plugin-log so all frontend logs
// appear in tracing (logcat on Android, stdout on desktop).
// See docs/LOGGING.md for architecture details.
const CONSOLE_FORWARDING_FLAG = '__floppa_console_forwarding_installed__'

function setupConsoleForwarding() {
  const globalObj = window as Window & { [CONSOLE_FORWARDING_FLAG]?: boolean }
  if (globalObj[CONSOLE_FORWARDING_FLAG]) return
  globalObj[CONSOLE_FORWARDING_FLAG] = true

  const forward = (
    fnName: 'log' | 'debug' | 'info' | 'warn' | 'error',
    logger: (message: string) => Promise<void>,
  ) => {
    const original = console[fnName]
    console[fnName] = (...args: unknown[]) => {
      original(...args)
      const message = args
        .map((arg) => {
          if (arg instanceof Error)
            return `${arg.name}: ${arg.message}\nStack: ${arg.stack || 'N/A'}`
          if (typeof arg === 'object' && arg !== null) {
            try {
              return JSON.stringify(arg)
            } catch {
              return '[Object]'
            }
          }
          return String(arg)
        })
        .join(' ')
      logger(message).catch(() => {})
    }
  }

  forward('log', trace)
  forward('debug', debug)
  forward('info', info)
  forward('warn', warn)
  forward('error', error)
}

setupConsoleForwarding()
void info('[web] Frontend initialized')

const app = createApp(App)

// Setup Pinia first (needed for auth store and Pinia Colada)
const pinia = createPinia()
pinia.use(piniaPluginPersistedstate)
app.use(pinia)
app.use(PiniaColada)

// Set active pinia so stores can be used outside component setup (Pinia 3 requirement)
setActivePinia(pinia)

// Setup i18n and Nuxt UI
app.use(createSharedI18n())
app.use(ui)

// Configure shared API client with auth interceptors
const authStore = useAuthStore()
const updateStore = useUpdateStore()

client.setConfig({ baseUrl: API_URL })
installApiInterceptors(client, authStore, {
  clientVersion: __APP_VERSION__,
  onUpgradeRequired: (body) =>
    updateStore.setForceUpdate({
      minVersion: body.min_version ?? 'unknown',
      message: body.message ?? 'Please update the app',
    }),
})

// Setup router with shared routes, overriding dashboard and login
const routes = createAppRoutes()
const overrides: Record<string, () => Promise<unknown>> = {
  login: () => Promise.resolve(ClientLoginView),
  dashboard: () => Promise.resolve(ClientDashboardView),
  info: () => Promise.resolve(ClientInfoView),
}

for (const route of routes) {
  if (route.name && typeof route.name === 'string' && overrides[route.name]) {
    route.component = overrides[route.name]
  }
}

// Add client-only routes (not in shared router)
routes.push({
  path: '/settings',
  name: 'settings',
  component: () => import('./views/SettingsView.vue'),
  meta: { requiresAuth: true },
})

const router = createRouter({
  history: createWebHistory(),
  routes,
})

installAuthGuard(router)

// Deep-link authentication
const processedDeepLinkCodes = new Set<string>()
const processingDeepLinkCodes = new Set<string>()

type SingleInstancePayload = {
  args: string[]
  cwd: string
}

const EXCHANGE_ATTEMPTS = 3
const EXCHANGE_RETRY_DELAY_MS = 1000

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

class DeepLinkExchangeError extends Error {
  constructor(
    /** HTTP status of the refusal, or `undefined` when the server was never reached. */
    readonly status: number | undefined,
    detail: string,
  ) {
    super(status === undefined ? `no response: ${detail}` : `HTTP ${status}: ${detail}`)
    this.name = 'DeepLinkExchangeError'
  }
}

/** Exchange with retries on transient (network/5xx) failures. 4xx means the code is
 * invalid or burned — retrying can't help, so those fail immediately. */
async function exchangeWithRetry(code: string) {
  for (let attempt = 1; ; attempt++) {
    // `throwOnError: false` keeps `response`: the thrown value would be the error body (or a
    // bare TypeError when offline), and neither carries the status this decision needs.
    const { data, error, response } = await exchangeTelegramLoginCode({
      body: { code },
      throwOnError: false,
    })
    if (data) return data

    // Offline, `error` is the fetch TypeError rather than an ApiError; both carry `message`.
    const failure = new DeepLinkExchangeError(response?.status, error?.message ?? 'unknown error')
    const transient = failure.status === undefined || failure.status >= 500
    if (!transient || attempt >= EXCHANGE_ATTEMPTS) {
      throw failure
    }
    void warn(
      `[web] Deep-link code exchange attempt ${attempt} failed, retrying: ${failure.message}`,
    )
    await sleep(EXCHANGE_RETRY_DELAY_MS)
  }
}

async function handleDeepLinkUrls(urls: string[]) {
  for (const rawUrl of urls) {
    const code = extractDeepLinkLoginCode(rawUrl)
    if (!code) {
      continue
    }
    if (processedDeepLinkCodes.has(code) || processingDeepLinkCodes.has(code)) {
      continue
    }

    processingDeepLinkCodes.add(code)
    const deepLinkAuth = useDeepLinkAuthStore()
    deepLinkAuth.start()
    try {
      const response = await exchangeWithRetry(code)
      authStore.setAuth(response.token, response.user)
      processedDeepLinkCodes.add(code)
      deepLinkAuth.finish(true)
      await router.push('/')
      void info('[web] Deep-link login completed.')
    } catch (e) {
      deepLinkAuth.finish(false)
      void error(`[web] Failed to exchange deep-link login code: ${String(e)}`)
    } finally {
      processingDeepLinkCodes.delete(code)
    }
  }
}

async function setupDeepLinkAuth() {
  if (!isTauri()) {
    return
  }

  try {
    // Register the live listeners BEFORE processing the startup URL: the startup exchange
    // can take a while (network), and a re-tap of the link in the browser during that window
    // must not be dropped.
    await onOpenUrl((urls) => {
      void handleDeepLinkUrls(urls)
    })

    await listen<SingleInstancePayload>('single-instance', (event) => {
      const urls =
        event.payload?.args?.filter(
          (arg) => typeof arg === 'string' && arg.startsWith('floppa://'),
        ) ?? []
      if (urls.length === 0) {
        return
      }
      void info('[web] Deep-link received from single-instance payload.')
      void handleDeepLinkUrls(urls)
    })

    void info('[web] Deep-link listener initialized.')

    const startupUrls = await getCurrent()
    if (startupUrls && startupUrls.length > 0) {
      await handleDeepLinkUrls(startupUrls)
    }
  } catch (e) {
    void error(`[web] Failed to initialize deep-link listener: ${String(e)}`)
  }
}

app.use(router)
void setupDeepLinkAuth()

// Subscribe to tunnel state at app scope, not from the card that displays it: the tunnel outlives
// any one screen, and a subscription tied to a component would drop updates whenever the user
// navigated away.
void useVpnStore().init()

// Set safe area CSS variables on Android (env(safe-area-inset-*) doesn't work in Android WebView)
if (isTauri()) {
  commands
    .getSafeAreaInsets()
    .then((result) => {
      if (result.status === 'ok') {
        document.documentElement.style.setProperty('--safe-area-inset-top', `${result.data.top}px`)
        document.documentElement.style.setProperty(
          '--safe-area-inset-bottom',
          `${result.data.bottom}px`,
        )
      }
    })
    .catch(() => {})
}

app.mount('#app')

// Check for voluntary updates (non-blocking)
void updateStore.checkForUpdates()

// Show changelog on first launch after update (only after user reaches an authenticated page)
{
  const removeHook = router.afterEach((to) => {
    if (to.meta.requiresAuth && authStore.isAuthenticated) {
      removeHook()
      updateStore.checkPostUpdateChangelog()
    }
  })
}
