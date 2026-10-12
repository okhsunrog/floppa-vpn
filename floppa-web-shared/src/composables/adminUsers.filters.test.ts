import { describe, expect, test } from 'vite-plus/test'
import type { UserSummary } from '../client/types.gen'
import { selectAdminUsers, type AdminUserFilters } from './adminUsers'

const now = Date.parse('2026-10-12T12:00:00Z')
const defaults: AdminUserFilters = {
  plan: 'all',
  subscription: 'all',
  expiring: false,
  sort: 'id',
  descending: false,
  now,
  locale: 'en',
}
const user = (id: number, changes: Partial<UserSummary> = {}): UserSummary => ({
  id,
  created_at: '2026-01-01T00:00:00Z',
  is_admin: false,
  peer_count: 0,
  has_vless: false,
  subscription_active: false,
  ...changes,
})
const rows = [
  user(1, {
    current_plan_id: 2,
    current_plan: 'Premium',
    subscription_active: true,
    subscription_expires_at: '2026-10-14T12:00:00Z',
  }),
  user(2, {
    current_plan_id: 2,
    current_plan: 'Premium',
    subscription_active: false,
    subscription_expires_at: '2026-10-10T12:00:00Z',
  }),
  user(3),
  user(4, { current_plan_id: 1, current_plan: 'Basic', subscription_active: true }),
  user(5, {
    current_plan_id: 1,
    current_plan: 'Basic',
    subscription_active: true,
    subscription_expires_at: '2026-10-20T12:00:00Z',
  }),
]
const ids = (filters: Partial<AdminUserFilters>) =>
  selectAdminUsers(rows, { ...defaults, ...filters }).map((row) => row.id)

describe('admin subscription lists', () => {
  test('intersects plan and subscription status without losing expired subscriptions', () => {
    expect(ids({ plan: '2', subscription: 'active' })).toEqual([1])
    expect(ids({ plan: '2', subscription: 'expired' })).toEqual([2])
    expect(ids({ subscription: 'none' })).toEqual([3])
    expect(ids({ subscription: 'active' })).toEqual([1, 4, 5])
  })
  test('expiring excludes permanent, expired, stale active and distant subscriptions', () => {
    expect(ids({ expiring: true })).toEqual([1])
    expect(
      selectAdminUsers(
        [
          user(6, {
            subscription_active: true,
            current_plan_id: 1,
            subscription_expires_at: '2026-10-11T12:00:00Z',
          }),
        ],
        { ...defaults, expiring: true },
      ),
    ).toEqual([])
  })
  test('sorts dates globally with empty dates last in either direction', () => {
    expect(ids({ sort: 'subscription_expires_at' })).toEqual([2, 1, 5, 3, 4])
    expect(ids({ sort: 'subscription_expires_at', descending: true })).toEqual([5, 1, 2, 3, 4])
    expect(ids({ sort: 'subscription_expires_at', descending: true }).slice(0, 2)).toEqual([5, 1])
  })
  test('sorts numbers numerically and preserves deterministic ordering on ties', () => {
    const sorted = selectAdminUsers(
      [user(3, { peer_count: 2 }), user(2, { peer_count: 11 }), user(1, { peer_count: 2 })],
      { ...defaults, sort: 'peer_count' },
    )
    expect(sorted.map((row) => row.id)).toEqual([1, 3, 2])
    expect(rows.map((row) => row.id)).toEqual([1, 2, 3, 4, 5])
  })
})
