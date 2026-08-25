import { watch } from 'vue'
import type { RouteRecordRaw, Router } from 'vue-router'
import { useAuthStore } from '../stores'
import { getTelegramInitData, getTelegramUserId } from '../utils/telegram'

export function createAppRoutes(): RouteRecordRaw[] {
  return [
    {
      path: '/login',
      name: 'login',
      component: () => import('../views/LoginView.vue'),
    },
    {
      // Public landing for logged-out visitors: description, plans, downloads, login CTA.
      path: '/welcome',
      name: 'welcome',
      component: () => import('../views/InfoView.vue'),
      props: { variant: 'landing' },
    },
    {
      path: '/',
      name: 'dashboard',
      component: () => import('../views/user/DashboardView.vue'),
      meta: { requiresAuth: true },
    },
    {
      // In-app Info tab for authenticated users (same content, no login CTA).
      path: '/info',
      name: 'info',
      component: () => import('../views/InfoView.vue'),
      props: { variant: 'tab' },
      meta: { requiresAuth: true },
    },
    {
      path: '/peers',
      name: 'peers',
      component: () => import('../views/user/PeersView.vue'),
      meta: { requiresAuth: true },
    },
    {
      path: '/account',
      name: 'account',
      component: () => import('../views/user/AccountView.vue'),
      meta: { requiresAuth: true },
    },
    {
      path: '/admin',
      name: 'admin-dashboard',
      component: () => import('../views/admin/DashboardView.vue'),
      meta: { requiresAuth: true, requiresAdmin: true },
    },
    {
      path: '/admin/peers',
      name: 'admin-peers',
      component: () => import('../views/admin/PeersView.vue'),
      meta: { requiresAuth: true, requiresAdmin: true },
    },
    {
      path: '/admin/users',
      name: 'admin-users',
      component: () => import('../views/admin/UsersView.vue'),
      meta: { requiresAuth: true, requiresAdmin: true },
    },
    {
      path: '/admin/users/:id',
      name: 'admin-user-detail',
      component: () => import('../views/admin/UserDetailView.vue'),
      meta: { requiresAuth: true, requiresAdmin: true },
    },
    {
      path: '/admin/plans',
      name: 'admin-plans',
      component: () => import('../views/admin/PlansView.vue'),
      meta: { requiresAuth: true, requiresAdmin: true },
    },
    {
      path: '/admin/installations',
      name: 'admin-installations',
      component: () => import('../views/admin/InstallationsView.vue'),
      meta: { requiresAuth: true, requiresAdmin: true },
    },
    {
      path: '/admin/vless',
      name: 'admin-vless',
      component: () => import('../views/admin/VlessPeersView.vue'),
      meta: { requiresAuth: true, requiresAdmin: true },
    },
  ]
}

export interface AuthGuardOptions {
  /**
   * Route name to send logged-out visitors to when they hit a protected route.
   * The web uses 'welcome' (the public landing); the Tauri client uses 'login'.
   */
  unauthenticatedRedirect?: string
}

/**
 * Installs the auth guard and the logout redirect.
 *
 * The guard covers navigation; the redirect covers the session ending *without* one — the
 * navbar Logout button, or the 401 interceptor in either app's `main.ts` wiping a dead token.
 * Both only call `auth.logout()`; this is the single place that then moves the user off a
 * protected page, so no view needs to know the app's logged-out landing route.
 */
export function installAuthGuard(router: Router, options: AuthGuardOptions = {}): void {
  const unauthenticatedRedirect = options.unauthenticatedRedirect ?? 'login'

  // Inside a Telegram Mini App, always go through /login: LoginView auto-logs in via initData
  // and forwards to the dashboard, so the user never sees the public landing. Outside Telegram,
  // logged-out visitors land on the configured page.
  const loggedOutRoute = () => ({
    name: getTelegramInitData() !== null ? 'login' : unauthenticatedRedirect,
  })

  // The store is only usable once Pinia is installed, which is after the router module has
  // been evaluated, so the watcher is attached on the first navigation rather than here.
  let redirectInstalled = false
  function installLogoutRedirect(auth: ReturnType<typeof useAuthStore>) {
    if (redirectInstalled) return
    redirectInstalled = true
    watch(
      () => auth.isAuthenticated,
      (authenticated) => {
        if (!authenticated && router.currentRoute.value.meta.requiresAuth) {
          void router.push(loggedOutRoute())
        }
      },
    )
  }

  router.beforeEach((to) => {
    const auth = useAuthStore()
    installLogoutRedirect(auth)

    // If in Mini App and a different Telegram account opened the app, force re-login.
    // `auth.telegramId` is only recorded for sessions that came in through Telegram; a
    // login+password session opened inside the Mini App has none, and that is not a
    // mismatch — treating it as one made credential login impossible inside Telegram.
    const tgUserId = getTelegramUserId()
    if (
      tgUserId !== null &&
      auth.isAuthenticated &&
      auth.telegramId !== null &&
      auth.telegramId !== tgUserId
    ) {
      auth.logout()
    }

    if (to.meta.requiresAuth && !auth.isAuthenticated) {
      return loggedOutRoute()
    }

    if (to.meta.requiresAdmin && !auth.isAdmin) {
      return { name: 'dashboard' }
    }

    if (to.name === 'login' && auth.isAuthenticated) {
      return { name: 'dashboard' }
    }
  })
}
