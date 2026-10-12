import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

// Each comparison worktree selects its own navigation; all use the real admin pages.
export const ADMIN_NAVIGATION: 'overview' | 'tabs' | 'sidebar' = 'sidebar'

export function useAdminNavigation() {
  const { t } = useI18n()
  return computed(() => [
    { label: t('nav.overview'), icon: 'i-lucide-chart-no-axes-combined', to: '/admin' },
    { label: t('nav.users'), icon: 'i-lucide-users', to: '/admin/users' },
    {
      label: t('adminWorkspace.subscriptions'),
      icon: 'i-lucide-ticket',
      to: '/admin/subscriptions',
    },
    { label: t('nav.plans'), icon: 'i-lucide-list', to: '/admin/plans' },
    { label: t('nav.peers'), icon: 'i-lucide-link', to: '/admin/peers' },
    { label: t('nav.vless'), icon: 'i-lucide-globe', to: '/admin/vless' },
    {
      label: t('nav.installations'),
      icon: 'i-lucide-monitor-smartphone',
      to: '/admin/installations',
    },
  ])
}
