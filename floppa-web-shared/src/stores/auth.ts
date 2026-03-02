import { ref, computed } from 'vue'
import { defineStore } from 'pinia'
import type { AuthUserInfo } from '../client/types.gen'

const TOKEN_KEY = 'floppa-token'
const USER_KEY = 'floppa-user'

export const useAuthStore = defineStore('auth', () => {
  const token = ref<string | null>(localStorage.getItem(TOKEN_KEY))
  const user = ref<AuthUserInfo | null>((() => {
    const stored = localStorage.getItem(USER_KEY)
    if (!stored) return null
    try {
      const parsed = JSON.parse(stored)
      if (typeof parsed?.id !== 'number') return null
      return parsed
    } catch {
      return null
    }
  })())

  const isAuthenticated = computed(() => !!token.value)
  const isAdmin = computed(() => user.value?.is_admin ?? false)

  function setAuth(newToken: string, newUser: AuthUserInfo) {
    token.value = newToken
    user.value = newUser
    localStorage.setItem(TOKEN_KEY, newToken)
    localStorage.setItem(USER_KEY, JSON.stringify(newUser))
  }

  function logout() {
    token.value = null
    user.value = null
    localStorage.removeItem(TOKEN_KEY)
    localStorage.removeItem(USER_KEY)
  }

  function getToken(): string | null {
    return token.value
  }

  return {
    token,
    user,
    isAuthenticated,
    isAdmin,
    setAuth,
    logout,
    getToken,
  }
})
