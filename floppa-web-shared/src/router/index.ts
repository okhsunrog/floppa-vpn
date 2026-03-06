import type { RouteRecordRaw, Router } from 'vue-router'
import { useAuthStore } from '../stores'

export function createAppRoutes(): RouteRecordRaw[] {
  return [
    {
      path: '/login',
      name: 'login',
      component: () => import('../views/LoginView.vue'),
    },
    {
      path: '/',
      name: 'dashboard',
      component: () => import('../views/user/DashboardView.vue'),
      meta: { requiresAuth: true },
    },
    {
      path: '/peers',
      name: 'peers',
      component: () => import('../views/user/PeersView.vue'),
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
  ]
}

/** Parse Telegram user ID from Mini App initData (URL-encoded). */
function getTelegramUserIdFromInitData(): number | null {
  try {
    const initData = (window as { Telegram?: { WebApp?: { initData?: string } } }).Telegram?.WebApp
      ?.initData
    if (!initData) return null
    const userJson = new URLSearchParams(initData).get('user')
    if (!userJson) return null
    const id = JSON.parse(userJson).id
    return typeof id === 'number' ? id : null
  } catch {
    return null
  }
}

export function installAuthGuard(router: Router): void {
  router.beforeEach((to) => {
    const auth = useAuthStore()

    // If in Mini App and a different Telegram account opened the app, force re-login
    const tgUserId = getTelegramUserIdFromInitData()
    if (tgUserId !== null && auth.isAuthenticated && auth.telegramId !== tgUserId) {
      auth.logout()
    }

    if (to.meta.requiresAuth && !auth.isAuthenticated) {
      return { name: 'login' }
    }

    if (to.meta.requiresAdmin && !auth.isAdmin) {
      return { name: 'dashboard' }
    }

    if (to.name === 'login' && auth.isAuthenticated) {
      return { name: 'dashboard' }
    }
  })
}
