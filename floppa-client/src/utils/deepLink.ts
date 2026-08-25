/**
 * The login code carried by a `floppa://auth?code=…` deep link, or `null` for any other URL.
 *
 * Both `floppa://auth` (host form) and `floppa:///auth` (path form) are accepted: which one the
 * OS hands over depends on the platform's URL parser.
 */
export function extractDeepLinkLoginCode(rawUrl: string): string | null {
  try {
    const parsedUrl = new URL(rawUrl)
    if (parsedUrl.protocol !== 'floppa:') {
      return null
    }

    const isAuthRoute = parsedUrl.hostname === 'auth' || parsedUrl.pathname === '/auth'
    if (!isAuthRoute) {
      return null
    }

    return parsedUrl.searchParams.get('code')
  } catch {
    return null
  }
}
