import { afterEach, beforeEach, describe, expect, test, vi } from 'vite-plus/test'
import { createPinia, setActivePinia } from 'pinia'
import { defineComponent } from 'vue'
import { createMemoryHistory, createRouter, type RouteRecordRaw, type Router } from 'vue-router'

import { useAuthStore } from '../stores'
import { createAppRoutes, installAuthGuard, type AuthGuardOptions } from './index'

// The store persists through localStorage, which node does not have.
function memoryStorage(): Storage {
  const data = new Map<string, string>()
  return {
    get length() {
      return data.size
    },
    key: (i) => [...data.keys()][i] ?? null,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => void data.set(k, String(v)),
    removeItem: (k) => void data.delete(k),
    clear: () => data.clear(),
  }
}

function fakeJwt(exp: number): string {
  const payload = btoa(JSON.stringify({ exp })).replace(/=+$/, '')
  return `eyJhbGciOiJIUzI1NiJ9.${payload}.sig`
}

const Stub = defineComponent({ render: () => null })

/** Production routes (names, paths, meta) with the lazy views swapped for a stub. */
function makeRouter(options?: AuthGuardOptions): Router {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: createAppRoutes().map((r): RouteRecordRaw => ({
      path: r.path,
      name: r.name,
      meta: r.meta,
      component: Stub,
    })),
  })
  installAuthGuard(router, options)
  return router
}

async function until(check: () => boolean) {
  for (let i = 0; i < 50 && !check(); i++) await new Promise((r) => setTimeout(r, 0))
  expect(check()).toBe(true)
}

function login(
  auth: ReturnType<typeof useAuthStore>,
  opts: { admin?: boolean; telegramId?: number },
) {
  auth.replaceToken(fakeJwt(Math.floor(Date.now() / 1000) + 3600))
  auth.user = {
    id: 1,
    username: 'u',
    first_name: null,
    last_name: null,
    is_admin: opts.admin ?? false,
  }
  if (opts.telegramId !== undefined) auth.telegramId = opts.telegramId
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('installAuthGuard', () => {
  test('sends logged-out visitors to the login route by default, or the configured one', async () => {
    const router = makeRouter()
    await router.push('/peers')
    expect(router.currentRoute.value.name).toBe('login')

    const web = makeRouter({ unauthenticatedRedirect: 'welcome' })
    await web.push('/peers')
    expect(web.currentRoute.value.name).toBe('welcome')
  })

  test('keeps non-admins out of admin routes and logged-in users off the login page', async () => {
    const router = makeRouter()
    login(useAuthStore(), {})
    await router.push('/admin')
    expect(router.currentRoute.value.name).toBe('dashboard')
    await router.push('/login')
    expect(router.currentRoute.value.name).toBe('dashboard')
  })

  test('lets admins into admin routes', async () => {
    const router = makeRouter()
    login(useAuthStore(), { admin: true })
    await router.push('/admin/users')
    expect(router.currentRoute.value.name).toBe('admin-users')
  })

  test('redirects off a protected route when the session ends without a navigation', async () => {
    const router = makeRouter({ unauthenticatedRedirect: 'welcome' })
    const auth = useAuthStore()
    login(auth, {})
    await router.push('/peers')
    expect(router.currentRoute.value.name).toBe('peers')

    auth.logout() // what the 401 interceptor and the Logout button do
    await until(() => router.currentRoute.value.name === 'welcome')
  })

  describe('inside a Telegram Mini App', () => {
    const initDataFor = (id: number) =>
      new URLSearchParams({ user: JSON.stringify({ id }), hash: 'x' }).toString()

    function openInMiniApp(telegramUserId: number) {
      vi.stubGlobal('window', { Telegram: { WebApp: { initData: initDataFor(telegramUserId) } } })
    }

    test('logs out a session that belongs to a different Telegram account', async () => {
      openInMiniApp(222)
      const router = makeRouter({ unauthenticatedRedirect: 'welcome' })
      const auth = useAuthStore()
      login(auth, { telegramId: 111 })
      await router.push('/peers')
      expect(auth.isAuthenticated).toBe(false)
      // Mini App users always go through /login (auto-login), never the public landing.
      expect(router.currentRoute.value.name).toBe('login')
    })

    test('keeps a login+password session, which has no Telegram id to compare', async () => {
      openInMiniApp(222)
      const router = makeRouter()
      const auth = useAuthStore()
      login(auth, {})
      await router.push('/peers')
      expect(auth.isAuthenticated).toBe(true)
      expect(router.currentRoute.value.name).toBe('peers')
    })
  })
})
