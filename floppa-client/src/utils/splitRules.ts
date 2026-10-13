import type { TunnelParams } from '../bindings'

/**
 * What the split-tunnelling banner has to say, given the settings and the tunnel.
 *
 * - `none` — the settings are what is running, or nothing is running and nothing is being built;
 * - `applying` — a tunnel is being built right now, with exactly these settings;
 * - `dirty` — what will be there does not route what the settings say, so someone has to ask for
 *   a rebuild.
 *
 * A pure function because the condition is the bug: it used to be `connected && running !==
 * settings`, which made the banner — the page's only feedback — vanish for the whole of a rebuild,
 * and made a change during one invisible. Three inputs decide it and every combination of them is
 * a case in the test beside this file.
 */
export type SplitBanner = 'none' | 'applying' | 'dirty'

export function splitBanner(args: {
  /** What the settings ask for, normalised the way `params()` normalises it. */
  settings: TunnelParams
  /** The rules the running tunnel was built with, when they are known. */
  running: TunnelParams | null
  /** The rules the actor was asked for, when there is an Up intent. */
  intent: TunnelParams | null
  /** Whether a cycle is in progress — a connect, a rebuild or a teardown. */
  busy: boolean
  /** Whether a tunnel is up right now. */
  connected: boolean
}): SplitBanner {
  const { settings, running, intent, busy, connected } = args
  // While anything is in flight the intent is what the tunnel will have; the running rules are
  // what it has until then, and during a rebuild they are precisely the ones being replaced.
  const target = busy ? (intent ?? running) : running
  if (!target) return 'none'
  if (!busy && !connected) return 'none'
  if (sameParams(target, settings)) return busy ? 'applying' : 'none'
  return 'dirty'
}

/** The actor's own "same tunnel" test, on the shape the snapshot publishes. */
export function sameParams(a: TunnelParams, b: TunnelParams): boolean {
  return (
    a.split_mode === b.split_mode &&
    (a.allow_lan ?? false) === (b.allow_lan ?? false) &&
    a.apps.length === b.apps.length &&
    a.apps.every((app, i) => app === b.apps[i])
  )
}
