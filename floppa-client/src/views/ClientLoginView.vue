<script setup lang="ts">
import { computed } from 'vue'
import { isTauri } from '@tauri-apps/api/core'
import { openUrl } from '@tauri-apps/plugin-opener'
import { LoginView } from 'floppa-web-shared/views'

const isTauriRuntime = isTauri()

const authMode = computed(() => (isTauriRuntime ? ('deep-link' as const) : ('widget' as const)))

const deepLinkLoginUrl = computed(() => {
  if (!isTauriRuntime) return undefined
  const baseApiUrl = (import.meta.env.VITE_API_URL as string).replace(/\/$/, '')
  const startUrl = new URL(`${baseApiUrl}/auth/telegram/start`)
  startUrl.searchParams.set('redirect_uri', 'floppa://auth')
  return startUrl.toString()
})

async function handleDeepLinkLogin(url: string) {
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
    @deep-link-login="handleDeepLinkLogin"
  />
</template>
