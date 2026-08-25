/**
 * Build-time configuration, read and validated once.
 *
 * `VITE_API_URL` is baked in at build time: release.yml sets it, and a local build reads it from
 * `floppa-client/.env` (see `.env.example`). Without it the client has no server to talk to, so
 * a build that lacks it fails here, at startup, naming the variable — rather than later, with a
 * request to `undefined/me` and nothing in the log to say why.
 */
function requireApiUrl(): string {
  const raw: unknown = import.meta.env.VITE_API_URL
  if (typeof raw !== 'string' || raw.trim() === '') {
    throw new Error(
      'VITE_API_URL is not set. Put it in floppa-client/.env (see .env.example) or export it ' +
        'before building; the client cannot reach a server without it.',
    )
  }
  return raw.trim().replace(/\/+$/, '')
}

/** Base URL of the server API, without a trailing slash (e.g. `https://host.example/api`). */
export const API_URL = requireApiUrl()

/**
 * Self-hosted update source: the server mirrors the latest release (binaries + metadata) at
 * `<origin>/downloads/`, so the update check, download and changelog are served from our own
 * origin instead of GitHub — insurance in case GitHub becomes unreachable for clients.
 */
export const DOWNLOADS_BASE = `${API_URL.replace(/\/api$/, '')}/downloads`
