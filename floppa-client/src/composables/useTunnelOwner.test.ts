import { beforeEach, describe, expect, it, vi } from 'vite-plus/test'
import type { TunnelOwner } from '../bindings'

const { getTunnelOwner } = vi.hoisted(() => ({
  getTunnelOwner: vi.fn<() => Promise<TunnelOwner>>(),
}))
vi.mock('../bindings', () => ({ commands: { getTunnelOwner } }))

beforeEach(() => {
  vi.resetModules()
  getTunnelOwner.mockReset()
})

describe('service ownership', () => {
  it('does not promise to disconnect an inaccessible service when the app quits', async () => {
    getTunnelOwner.mockResolvedValue({
      kind: 'service_unavailable',
      problem: { kind: 'not_permitted' },
    })
    const { useTunnelOwner } = await import('./useTunnelOwner')
    const view = useTunnelOwner()
    await view.refresh()
    expect(view.lockedOut()).toBe(true)
    expect(view.quittingDisconnects()).toBe(false)
  })

  it('refreshes health after the service is restarted without changing ownership', async () => {
    getTunnelOwner.mockResolvedValueOnce({
      kind: 'service_unavailable',
      problem: { kind: 'wrong_version', theirs: 999 },
    })
    const { useTunnelOwner } = await import('./useTunnelOwner')
    const view = useTunnelOwner()
    await view.refresh()
    expect(view.owner.value?.kind).toBe('service_unavailable')
    getTunnelOwner.mockResolvedValue({ kind: 'service' })
    await view.refresh()
    expect(view.owner.value?.kind).toBe('service')
    expect(view.quittingDisconnects()).toBe(false)
  })

  it('shares an in-flight health query across consumers', async () => {
    let answer!: (owner: TunnelOwner) => void
    getTunnelOwner.mockImplementation(
      () =>
        new Promise((resolve) => {
          answer = resolve
        }),
    )
    const { useTunnelOwner } = await import('./useTunnelOwner')
    const first = useTunnelOwner()
    const second = useTunnelOwner()
    const pending = second.refresh()
    expect(getTunnelOwner).toHaveBeenCalledTimes(1)
    answer({ kind: 'in_process', reason: { kind: 'not_installed' } })
    await pending
    expect(first.quittingDisconnects()).toBe(true)
    expect(second.owner.value?.kind).toBe('in_process')
  })
})
