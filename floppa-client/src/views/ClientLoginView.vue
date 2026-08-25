<script setup lang="ts">
import { computed } from 'vue'
import { isTauri } from '@tauri-apps/api/core'
import { openUrl } from '@tauri-apps/plugin-opener'
import { LoginView } from 'floppa-web-shared/views'
import { useDeepLinkAuthStore } from '../stores/deepLinkAuthStore'
import { API_URL } from '../config'

const isTauriRuntime = isTauri()
const deepLinkAuth = useDeepLinkAuthStore()

const authMode = computed(() => (isTauriRuntime ? ('deep-link' as const) : ('widget' as const)))

const deepLinkLoginUrl = computed(() => {
  if (!isTauriRuntime) return undefined
  const startUrl = new URL(`${API_URL}/auth/telegram/start`)
  startUrl.searchParams.set('redirect_uri', 'floppa://auth')
  return startUrl.toString()
})

async function handleDeepLinkLogin(url: string) {
  // A fresh attempt supersedes any previous failure.
  deepLinkAuth.reset()
  try {
    await openUrl(url)
  } catch (e) {
    console.error('Failed to open browser login:', e)
  }
}
</script>

<template>
  <LoginView
    :auth-mode="authMode"
    :deep-link-login-url="deepLinkLoginUrl"
    :deep-link-busy="deepLinkAuth.exchanging"
    :deep-link-failed="deepLinkAuth.failed"
    @deep-link-login="handleDeepLinkLogin"
  />
</template>
