import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import piniaPluginPersistedstate from 'pinia-plugin-persistedstate'
import { PiniaColada } from '@pinia/colada'
import { createRouter, createWebHistory } from 'vue-router'
import ui from '@nuxt/ui/vue-plugin'
import { isTauri } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrent, onOpenUrl } from '@tauri-apps/plugin-deep-link'
import { useAuthStore } from 'floppa-web-shared'
import { useUpdateStore } from './stores/updateStore'
import { useDeepLinkAuthStore } from './stores/deepLinkAuthStore'
import { commands } from './bindings'
import { client } from 'floppa-web-shared/client/client.gen'
import { exchangeTelegramLoginCode } from 'floppa-web-shared/client/sdk.gen'
import { createAppRoutes, installAuthGuard } from 'floppa-web-shared/router'
import { trace, debug, info, warn, error } from '@tauri-apps/plugin-log'
import { i18n } from './i18n'

import './styles.css'
import App from './App.vue'
import ClientLoginView from './views/ClientLoginView.vue'
import ClientDashboardView from './views/ClientDashboardView.vue'
import ClientInfoView from './views/ClientInfoView.vue'

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
app.use(i18n)
app.use(ui)

// Configure shared API client with auth interceptors
const API_URL = import.meta.env.VITE_API_URL as string
const authStore = useAuthStore()

client.setConfig({ baseUrl: API_URL })

client.interceptors.request.use((request) => {
  request.headers.set('X-Client-Version', __APP_VERSION__)
  const token = authStore.getToken()
  if (token) {
    request.headers.set('Authorization', `Bearer ${token}`)
  }
  return request
})

const updateStore = useUpdateStore()

client.interceptors.response.use(async (response, request) => {
  // Sliding session: the server attaches a fresh JWT once the current one is a day old.
  const refreshed = response.headers.get('x-refreshed-token')
  if (refreshed) {
    authStore.replaceToken(refreshed)
  }
  // Only a rejected *authenticated* request means our session is dead; public endpoints
  // (e.g. a failed login-code exchange) also return 401 and must not wipe the session.
  if (response.status === 401 && request.headers.has('Authorization')) {
    authStore.logout()
  }
  if (response.status === 426) {
    try {
      const body = await response.clone().json()
      updateStore.setForceUpdate({ minVersion: body.min_version, message: body.message })
    } catch {
      updateStore.setForceUpdate({ minVersion: 'unknown', message: 'Please update the app' })
    }
  }
  return response
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

function extractDeepLinkLoginCode(rawUrl: string): string | null {
  try {
    const parsedUrl = new URL(rawUrl)
    if (parsedUrl.protocol !== 'floppa:') {
      return null
    }

    const isAuthRoute = parsedUrl.hostname === 'auth' || parsedUrl.pathname === '/auth'
    if (!isAuthRoute) {
      return null
    }

    return parsedUrl.searchParams.get('code')
  } catch {
    return null
  }
}

const EXCHANGE_ATTEMPTS = 3
const EXCHANGE_RETRY_DELAY_MS = 1000

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/** Exchange with retries on transient (network/5xx) failures. 4xx means the code is
 * invalid or burned — retrying can't help, so those fail immediately. */
async function exchangeWithRetry(code: string) {
  for (let attempt = 1; ; attempt++) {
    try {
      const { data: response } = await exchangeTelegramLoginCode({
        body: { code },
        throwOnError: true,
      })
      return response
    } catch (e) {
      const status = (e as { status?: number })?.status
      const transient = status === undefined || status >= 500
      if (!transient || attempt >= EXCHANGE_ATTEMPTS) {
        throw e
      }
      void warn(`[web] Deep-link code exchange attempt ${attempt} failed, retrying: ${String(e)}`)
      await sleep(EXCHANGE_RETRY_DELAY_MS)
    }
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
