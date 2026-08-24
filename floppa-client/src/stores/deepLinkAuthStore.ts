import { defineStore } from 'pinia'
import { ref } from 'vue'

/**
 * Tracks the browser → app deep-link login exchange so the login screen can show
 * progress and failures instead of silently sitting on the login form.
 */
export const useDeepLinkAuthStore = defineStore('deepLinkAuth', () => {
  const exchanging = ref(false)
  const failed = ref(false)

  function start() {
    exchanging.value = true
    failed.value = false
  }

  function finish(ok: boolean) {
    exchanging.value = false
    failed.value = !ok
  }

  function reset() {
    exchanging.value = false
    failed.value = false
  }

  return { exchanging, failed, start, finish, reset }
})
