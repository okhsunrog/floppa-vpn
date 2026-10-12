import { expect, test } from 'vite-plus/test'
import type { UserSummary } from '../client/types.gen'
import { selectAdminUsers, type AdminUserFilters } from './adminUsers'

const now = Date.parse('2026-10-12T00:00:00Z')
const filters: AdminUserFilters = {
  plan: 'all',
  subscription: 'all',
  expiring: false,
  sort: 'id',
  descending: false,
  now,
  locale: 'en',
}
function user(id: number, patch: Partial<UserSummary> = {}): UserSummary {
  return {
    id,
    created_at: '2026-10-01T00:00:00Z',
    has_vless: false,
    is_admin: false,
    peer_count: 0,
    subscription_active: false,
    ...patch,
  }
}
const rows = [
  user(1),
  user(2, {
    current_plan_id: 1,
    subscription_active: true,
    subscription_expires_at: '2026-10-13T00:00:00Z',
  }),
  user(3, { current_plan_id: 1, subscription_expires_at: '2026-10-11T00:00:00Z' }),
  user(4, { current_plan_id: 2, subscription_active: true }),
]

test('distinguishes expired, permanent and absent subscriptions and combines plan filters', () => {
  expect(selectAdminUsers(rows, { ...filters, subscription: 'none' }).map((u) => u.id)).toEqual([1])
  expect(selectAdminUsers(rows, { ...filters, subscription: 'expired' }).map((u) => u.id)).toEqual([
    3,
  ])
  expect(selectAdminUsers(rows, { ...filters, subscription: 'active' }).map((u) => u.id)).toEqual([
    2, 4,
  ])
  expect(
    selectAdminUsers(rows, { ...filters, plan: '1', subscription: 'active' }).map((u) => u.id),
  ).toEqual([2])
})

test('expiry window excludes permanent, already elapsed and distant subscriptions', () => {
  const stale = user(5, {
    current_plan_id: 1,
    subscription_active: true,
    subscription_expires_at: '2026-10-11T00:00:00Z',
  })
  const distant = user(6, {
    current_plan_id: 1,
    subscription_active: true,
    subscription_expires_at: '2026-10-20T00:00:00Z',
  })
  expect(
    selectAdminUsers([...rows, stale, distant], { ...filters, expiring: true }).map((u) => u.id),
  ).toEqual([2])
})

test('sorts dates in both directions with empty dates last without mutating input', () => {
  expect(
    selectAdminUsers(rows, { ...filters, sort: 'subscription_expires_at' }).map((u) => u.id),
  ).toEqual([3, 2, 1, 4])
  expect(
    selectAdminUsers(rows, { ...filters, sort: 'subscription_expires_at', descending: true }).map(
      (u) => u.id,
    ),
  ).toEqual([2, 3, 1, 4])
  expect(rows.map((u) => u.id)).toEqual([1, 2, 3, 4])
})
