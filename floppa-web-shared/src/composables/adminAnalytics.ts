import type { Analytics } from '../client/types.gen'

export const USAGE_THRESHOLD = 1024 * 1024
export function summarizeAnalytics(data: Analytics) {
  const paid = data.events.filter((event) => event.kind === 'first' || event.kind === 'repeat')
  return {
    first: new Set(paid.filter((e) => e.kind === 'first').map((e) => e.user_id)).size,
    repeat: new Set(paid.filter((e) => e.kind === 'repeat').map((e) => e.user_id)).size,
    buyers: new Set(paid.map((e) => e.user_id)).size,
    payments: paid.length,
    revenue: paid.reduce((sum, e) => sum + e.amount, 0),
    manual: data.events.filter((e) => e.kind === 'admin_grant').length,
    manualUsers: new Set(data.events.filter((e) => e.kind === 'admin_grant').map((e) => e.user_id))
      .size,
    credit: data.events.filter((e) => e.kind === 'credit').length,
    usage: data.usage?.filter((u) => u.download + u.upload >= USAGE_THRESHOLD) ?? null,
  }
}

export function paymentTimeline(data: Analytics, from: string, to: string, weekly = false) {
  const start = Date.parse(`${from}T00:00:00Z`)
  const end = Date.parse(`${to}T00:00:00Z`)
  const step = weekly || (end - start) / 86400000 > 31 ? 7 : 1
  const bins: { from: string; to: string; first: number; repeat: number }[] = []
  for (let at = start; at <= end; at += step * 86400000) {
    const until = Math.min(end, at + (step - 1) * 86400000)
    bins.push({
      from: new Date(at).toISOString().slice(0, 10),
      to: new Date(until).toISOString().slice(0, 10),
      first: 0,
      repeat: 0,
    })
  }
  for (const e of data.events) {
    if (e.kind !== 'first' && e.kind !== 'repeat') continue
    const index = Math.floor((Date.parse(e.at) - start) / (step * 86400000))
    const bin = bins[index]
    if (bin && e.at.slice(0, 10) <= to) bin[e.kind]++
  }
  return bins
}
