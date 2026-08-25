import type { Protocol } from '../bindings'

/**
 * Probe priority used until the user reorders it, lowest first. AmneziaWG leads because plain
 * WireGuard is DPI-blocked on the networks this client targets.
 *
 * A `Record` and not an array: `Protocol` is generated from Rust, so this stops compiling the
 * moment a protocol is added there and nobody has said where it belongs. As a list it would
 * simply have been short, and the missing protocol would have been unreachable in auto-select.
 */
const DEFAULT_PRIORITY: Record<Protocol, number> = {
  amneziawg: 0,
  wireguard: 1,
  vless: 2,
}

export const DEFAULT_PROTOCOL_ORDER: readonly Protocol[] = (
  Object.keys(DEFAULT_PRIORITY) as Protocol[]
).sort((a, b) => DEFAULT_PRIORITY[a] - DEFAULT_PRIORITY[b])

const KNOWN_PROTOCOLS = new Set<string>(DEFAULT_PROTOCOL_ORDER)

export function isProtocol(value: unknown): value is Protocol {
  return typeof value === 'string' && KNOWN_PROTOCOLS.has(value)
}

/** Persisted orders are user data from an older build: drop anything that is no longer a protocol,
 *  then append protocols added since, so the list is always exactly the known set. */
export function sanitizeProtocolOrder(stored: unknown): Protocol[] {
  const kept = Array.isArray(stored) ? stored.filter(isProtocol) : []
  const deduped = [...new Set(kept)]
  return [...deduped, ...DEFAULT_PROTOCOL_ORDER.filter((p) => !deduped.includes(p))]
}
