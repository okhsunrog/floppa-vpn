import { describe, expect, it } from 'vite-plus/test'
import type { TunnelParams } from '../bindings'
import { sameParams, splitBanner } from './splitRules'

const exclude = (...apps: string[]): TunnelParams => ({ split_mode: 'exclude', apps })
const all: TunnelParams = { split_mode: 'all', apps: [] }

/** The settings, the running tunnel and the intent, with the rest of the arguments defaulted. */
function banner(args: {
  settings: TunnelParams
  running?: TunnelParams | null
  intent?: TunnelParams | null
  busy?: boolean
  connected?: boolean
}) {
  return splitBanner({
    settings: args.settings,
    running: args.running ?? null,
    intent: args.intent ?? null,
    busy: args.busy ?? false,
    connected: args.connected ?? false,
  })
}

describe('splitBanner', () => {
  it('says nothing when nothing is running and nothing is being built', () => {
    expect(banner({ settings: exclude('com.a') })).toBe('none')
  })

  it('says nothing when the running tunnel routes what the settings say', () => {
    expect(banner({ settings: exclude('com.a'), running: exclude('com.a'), connected: true })).toBe(
      'none',
    )
  })

  it('asks for a reconnect when the running tunnel does not', () => {
    expect(
      banner({ settings: exclude('com.a', 'com.b'), running: exclude('com.a'), connected: true }),
    ).toBe('dirty')
  })

  it('says nothing about a tunnel whose rules are unknown', () => {
    // An adopted tunnel whose owner does not report them: there is nothing to compare, and
    // guessing would nag about a tunnel that may well be correct.
    expect(banner({ settings: exclude('com.a'), running: null, connected: true })).toBe('none')
  })

  // The regression this file exists for: the old condition was `connected && running !==
  // settings`, so everything below reported 'none' — on a page whose only feedback is the banner.
  it('says it is applying while the tunnel is being rebuilt with these very settings', () => {
    expect(
      banner({
        settings: exclude('com.a'),
        running: all,
        intent: exclude('com.a'),
        busy: true,
      }),
    ).toBe('applying')
  })

  it('asks again when the settings change while a rebuild is in flight', () => {
    expect(
      banner({
        settings: all,
        running: all,
        intent: exclude('com.a'),
        busy: true,
      }),
    ).toBe('dirty')
  })

  it('reads the intent, not the tunnel, while a rebuild is in flight', () => {
    // The running tunnel still matches the settings here, and the banner must *not* be quiet: the
    // rebuild under way is about to replace it with something else.
    expect(
      banner({
        settings: exclude('com.a'),
        running: exclude('com.a'),
        intent: all,
        busy: true,
      }),
    ).toBe('dirty')
  })

  it('falls back to the running rules while busy with no intent params', () => {
    // A teardown, or the bootstrap adoption intent, which names no rules.
    expect(banner({ settings: exclude('com.a'), running: exclude('com.a'), busy: true })).toBe(
      'applying',
    )
  })
})

describe('sameParams', () => {
  it('compares the mode and the list in order', () => {
    expect(sameParams(exclude('com.a', 'com.b'), exclude('com.a', 'com.b'))).toBe(true)
    expect(sameParams(exclude('com.a', 'com.b'), exclude('com.b', 'com.a'))).toBe(false)
    expect(sameParams(exclude(), all)).toBe(false)
  })
})
