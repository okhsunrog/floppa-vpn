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

export function installAuthGuard(router: Router): void {
  router.beforeEach((to) => {
    const auth = useAuthStore()

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
