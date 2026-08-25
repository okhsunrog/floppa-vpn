/** The release triple, ignoring any prerelease or build suffix. `null` if there isn't one. */
export function parseSemver(version: string): [number, number, number] | null {
  const m = /^\s*(\d+)\.(\d+)\.(\d+)/.exec(version)
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null
}

/**
 * Compare two versions, or `null` when either cannot be read as one.
 *
 * The previous version did `split('.').map(Number)`, so anything non-numeric produced `NaN`,
 * every comparison against `NaN` was false, and both callers read that as "the remote one is
 * newer" — a permanent update banner and the changelog on every launch. `latest.json` comes off
 * the network, so its shape is not ours to assume.
 */
export function compareSemver(a: string, b: string): number | null {
  const pa = parseSemver(a)
  const pb = parseSemver(b)
  if (!pa || !pb) return null
  for (let i = 0; i < 3; i++) {
    const diff = pa[i]! - pb[i]!
    if (diff !== 0) return diff
  }
  return 0
}
