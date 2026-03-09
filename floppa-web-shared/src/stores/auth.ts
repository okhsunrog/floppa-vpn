import { ref, computed } from 'vue'
import { defineStore } from 'pinia'
import type { AuthUserInfo } from '../client/types.gen'

const TOKEN_KEY = 'floppa-token'
const USER_KEY = 'floppa-user'
const AVATAR_KEY = 'floppa-avatar'
const TELEGRAM_ID_KEY = 'floppa-telegram-id'

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
  const avatarUrl = computed(() => cachedAvatar.value ?? user.value?.photo_url ?? undefined)

  const isAuthenticated = computed(() => !!token.value)
  const isAdmin = computed(() => user.value?.is_admin ?? false)

  function cacheAvatar(url: string) {
    fetch(url, { mode: 'cors', signal: AbortSignal.timeout(10_000) })
      .then((r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`)
        return r.blob()
      })
      .then((blob) => {
        const reader = new FileReader()
        reader.onloadend = () => {
          const dataUrl = reader.result as string
          cachedAvatar.value = dataUrl
          localStorage.setItem(AVATAR_KEY, dataUrl)
        }
        reader.readAsDataURL(blob)
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
    if (newUser.photo_url) cacheAvatar(newUser.photo_url)
  }

  function logout() {
    token.value = null
    user.value = null
    cachedAvatar.value = null
    localStorage.removeItem(TOKEN_KEY)
    localStorage.removeItem(USER_KEY)
    localStorage.removeItem(AVATAR_KEY)
    telegramId.value = null
    localStorage.removeItem(TELEGRAM_ID_KEY)
  }

  // Try to cache avatar on startup if we have a photo_url but no cached version
  if (user.value?.photo_url && !cachedAvatar.value) {
    cacheAvatar(user.value.photo_url)
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
    logout,
    getToken,
  }
})
