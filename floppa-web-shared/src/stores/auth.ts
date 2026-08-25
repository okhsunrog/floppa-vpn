import { ref, computed } from 'vue'
import { defineStore } from 'pinia'
import type { AuthUserInfo } from '../client/types.gen'
import { getMyAvatar } from '../client/sdk.gen'

const TOKEN_KEY = 'floppa-token'
const USER_KEY = 'floppa-user'
const AVATAR_KEY = 'floppa-avatar'
// Timestamp of the last confirmed "this user has no avatar", so users without a Telegram photo
// don't cost a retry chain of avatar requests on every start. Re-checked after the TTL so a
// newly added photo still shows up within the hour.
const AVATAR_MISSING_KEY = 'floppa-avatar-missing-at'
const AVATAR_MISSING_TTL_MS = 60 * 60 * 1000
const TELEGRAM_ID_KEY = 'floppa-telegram-id'

/** The JWT `exp` claim (seconds since epoch), or null if absent/unreadable. */
function jwtExp(jwt: string): number | null {
  const payload = jwt.split('.')[1]
  if (!payload) return null
  try {
    const json = atob(payload.replace(/-/g, '+').replace(/_/g, '/'))
    const exp = (JSON.parse(json) as { exp?: unknown }).exp
    return typeof exp === 'number' ? exp : null
  } catch {
    return null
  }
}

/** True only when the token carries an `exp` claim that is already in the past. */
function isTokenExpired(jwt: string): boolean {
  const exp = jwtExp(jwt)
  return exp !== null && exp * 1000 <= Date.now()
}

export const useAuthStore = defineStore('auth', () => {
  const token = ref<string | null>(localStorage.getItem(TOKEN_KEY))
  const user = ref<AuthUserInfo | null>(
    (() => {
      const stored = localStorage.getItem(USER_KEY)
      if (!stored) return null
      try {
        const parsed = JSON.parse(stored)
        if (typeof parsed?.id !== 'number') return null
        return parsed
      } catch {
        return null
      }
    })(),
  )

  const telegramId = ref<number | null>(
    (() => {
      const stored = localStorage.getItem(TELEGRAM_ID_KEY)
      if (!stored) return null
      const n = Number(stored)
      return Number.isFinite(n) ? n : null
    })(),
  )

  const cachedAvatar = ref<string | null>(localStorage.getItem(AVATAR_KEY))
  // Avatars are served from our own origin (server-cached); the raw Telegram photo_url is
  // unreachable from clients in Russia, so it's not used as a fallback.
  const avatarUrl = computed(() => cachedAvatar.value ?? undefined)

  const isAuthenticated = computed(() => !!token.value && !isTokenExpired(token.value))
  const isAdmin = computed(() => user.value?.is_admin ?? false)

  function avatarKnownMissing(): boolean {
    const at = Number(localStorage.getItem(AVATAR_MISSING_KEY))
    return Number.isFinite(at) && at > 0 && Date.now() - at < AVATAR_MISSING_TTL_MS
  }

  /// Fetch the current user's avatar from the server (Bearer-authed) and cache it as a data URL.
  /// The first request triggers a background download from Telegram (getUserProfilePhotos →
  /// getFile → download), which can take ~10s, so retry generously (≈24s window) before giving up.
  /// Once the retries are exhausted the miss is remembered (see AVATAR_MISSING_KEY).
  function cacheAvatar(retries = 8) {
    getMyAvatar({ parseAs: 'blob' })
      .then(({ data, response }) => {
        if (!response) return
        if (response.status === 404) {
          if (retries > 0) setTimeout(() => cacheAvatar(retries - 1), 3000)
          else localStorage.setItem(AVATAR_MISSING_KEY, String(Date.now()))
          return
        }
        if (!response.ok || !(data instanceof Blob)) return
        const reader = new FileReader()
        reader.onloadend = () => {
          const dataUrl = reader.result as string
          cachedAvatar.value = dataUrl
          localStorage.setItem(AVATAR_KEY, dataUrl)
          localStorage.removeItem(AVATAR_MISSING_KEY)
        }
        reader.readAsDataURL(data)
      })
      .catch(() => {})
  }

  function setAuth(newToken: string, newUser: AuthUserInfo, newTelegramId?: number) {
    token.value = newToken
    user.value = newUser
    localStorage.setItem(TOKEN_KEY, newToken)
    localStorage.setItem(USER_KEY, JSON.stringify(newUser))
    if (newTelegramId !== undefined) {
      telegramId.value = newTelegramId
      localStorage.setItem(TELEGRAM_ID_KEY, String(newTelegramId))
    }
    // A fresh login may be a different user: forget the previous "no avatar" verdict.
    localStorage.removeItem(AVATAR_MISSING_KEY)
    cacheAvatar()
  }

  /** Swap in a refreshed JWT without touching the user payload (sliding session). */
  function replaceToken(newToken: string) {
    token.value = newToken
    localStorage.setItem(TOKEN_KEY, newToken)
  }

  function logout() {
    token.value = null
    user.value = null
    cachedAvatar.value = null
    localStorage.removeItem(TOKEN_KEY)
    localStorage.removeItem(USER_KEY)
    localStorage.removeItem(AVATAR_KEY)
    localStorage.removeItem(AVATAR_MISSING_KEY)
    telegramId.value = null
    localStorage.removeItem(TELEGRAM_ID_KEY)
  }

  // Purge a token that expired since the last session, so the guard doesn't treat a
  // stale token as authenticated (and flash authed UI) until the first 401.
  if (token.value && isTokenExpired(token.value)) {
    logout()
  }

  // Refresh the avatar from the server on startup when authenticated but not yet cached, unless
  // the last attempt established that there is none to fetch.
  if (token.value && !cachedAvatar.value && !avatarKnownMissing()) {
    cacheAvatar()
  }

  function getToken(): string | null {
    return token.value
  }

  return {
    token,
    user,
    telegramId,
    isAuthenticated,
    isAdmin,
    avatarUrl,
    setAuth,
    replaceToken,
    logout,
    getToken,
  }
})
