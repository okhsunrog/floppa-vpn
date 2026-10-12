import type { UserSummary } from '../client/types.gen'

export type SubscriptionFilter = 'all' | 'active' | 'expired' | 'none'
export interface AdminUserFilters {
  plan: string
  subscription: SubscriptionFilter
  expiring: boolean
  sort: string
  descending: boolean
  now: number
  locale: string
}

export function selectAdminUsers(users: UserSummary[], filters: AdminUserFilters): UserSummary[] {
  const collator = new Intl.Collator(filters.locale, { numeric: true, sensitivity: 'base' })
  const value = (user: UserSummary): string | number | null => {
    switch (filters.sort) {
      case 'username':
        return [user.first_name, user.last_name].filter(Boolean).join(' ') || user.username || ''
      case 'current_plan':
        return user.current_plan ?? null
      case 'subscription_expires_at':
        return user.subscription_expires_at ? Date.parse(user.subscription_expires_at) : null
      case 'created_at':
        return Date.parse(user.created_at)
      case 'peer_count':
        return user.peer_count
      case 'is_admin':
        return Number(user.is_admin)
      case 'has_vless':
        return Number(user.has_vless)
      case 'client_version':
        return user.client_version ?? null
      case 'telegram_id':
        return user.telegram_id ?? null
      default:
        return user.id
    }
  }
  return users
    .filter((user) => {
      if (filters.plan !== 'all' && String(user.current_plan_id) !== filters.plan) return false
      const status =
        user.current_plan_id == null ? 'none' : user.subscription_active ? 'active' : 'expired'
      if (filters.subscription !== 'all' && status !== filters.subscription) return false
      if (
        filters.expiring &&
        (!user.subscription_active ||
          !user.subscription_expires_at ||
          Date.parse(user.subscription_expires_at) <= filters.now ||
          Date.parse(user.subscription_expires_at) > filters.now + 7 * 86400000)
      )
        return false
      return true
    })
    .sort((a, b) => {
      const av = value(a),
        bv = value(b)
      // Empty fields and permanent subscriptions stay at the bottom in either direction.
      if (av == null) return bv == null ? a.id - b.id : 1
      if (bv == null) return -1
      const compared =
        typeof av === 'number' && typeof bv === 'number'
          ? av - bv
          : collator.compare(String(av), String(bv))
      return compared ? compared * (filters.descending ? -1 : 1) : a.id - b.id
    })
}
