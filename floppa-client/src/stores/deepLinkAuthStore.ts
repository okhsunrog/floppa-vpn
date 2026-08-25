import { defineStore } from 'pinia'
import { ref } from 'vue'

/**
 * Tracks the browser → app deep-link login exchange so the login screen can show
 * progress and failures instead of silently sitting on the login form.
 */
export const useDeepLinkAuthStore = defineStore('deepLinkAuth', () => {
  const exchanging = ref(false)
  const failed = ref(false)
  /** Why the last exchange failed, already worded for the user; null when it did not fail. */
  const failureDetail = ref<string | null>(null)

  function start() {
    exchanging.value = true
    failed.value = false
    failureDetail.value = null
  }

  function succeed() {
    exchanging.value = false
    failed.value = false
    failureDetail.value = null
  }

  function fail(detail: string | null) {
    exchanging.value = false
    failed.value = true
    failureDetail.value = detail
  }

  function reset() {
    exchanging.value = false
    failed.value = false
    failureDetail.value = null
  }

  return { exchanging, failed, failureDetail, start, succeed, fail, reset }
})
