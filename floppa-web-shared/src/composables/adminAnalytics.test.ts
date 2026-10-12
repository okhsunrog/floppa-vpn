import { describe, it, expect } from 'vite-plus/test'
import { summarizeAnalytics, paymentTimeline } from './adminAnalytics'
import type { Analytics } from '../client/types.gen'
const event = (kind: string, user_id: number, amount = 0) => ({
  id: `${kind}-${user_id}`,
  kind,
  user_id,
  amount,
  name: 'Test',
  plan: 'Premium',
  at: '2026-10-10T12:00:00Z',
})
describe('subscription analytics', () => {
  it('keeps people, transactions, credit and manual grants distinct', () => {
    const data: Analytics = {
      events: [
        event('first', 1, 100),
        event('repeat', 1, 150),
        event('repeat', 1, 200),
        event('admin_grant', 1),
        event('admin_grant', 2),
        event('credit', 3),
        event('trial', 4),
      ],
      access: [],
      usage: null,
    }
    const totals = summarizeAnalytics(data)
    expect(totals).toMatchObject({
      first: 1,
      repeat: 1,
      buyers: 1,
      payments: 3,
      revenue: 450,
      manual: 2,
      manualUsers: 2,
      credit: 1,
      usage: null,
    })
    const bins = paymentTimeline(data, '2026-10-09', '2026-10-11')
    expect(bins.map((b) => [b.first, b.repeat])).toEqual([
      [0, 0],
      [1, 2],
      [0, 0],
    ])
  })
  it('does not equate missing telemetry, idle counters and meaningful usage', () => {
    const data: Analytics = {
      events: [],
      access: [],
      usage: [
        { user_id: 1, download: 1024, upload: 20 },
        { user_id: 2, download: 1024 * 1024, upload: 0 },
      ],
    }
    expect(summarizeAnalytics(data).usage?.map((u) => u.user_id)).toEqual([2])
    expect(summarizeAnalytics({ ...data, usage: [] }).usage).toEqual([])
  })
})
