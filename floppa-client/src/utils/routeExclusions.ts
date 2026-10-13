/** Match the core's IPv4 policy format before persisting a rule. */
export function normalizeDestination(raw: string): string | null {
  const match = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?:\/(\d{1,2}))?$/.exec(raw.trim())
  if (!match) return null
  const octets = match.slice(1, 5).map(Number)
  const prefix = match[5] === undefined ? 32 : Number(match[5])
  if (octets.some((v) => v > 255) || prefix > 32) return null
  const address = octets.reduce((a, v) => (a << 8) | v, 0) >>> 0
  const mask = prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0
  const network = (address & mask) >>> 0
  return `${[24, 16, 8, 0].map((n) => (network >>> n) & 255).join('.')}/${prefix}`
}
