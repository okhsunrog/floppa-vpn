<script setup lang="ts">
import { onMounted, ref } from 'vue'
import type { TelegramAuthData } from '../client/types.gen'

const props = defineProps<{
  botName: string
  size?: 'small' | 'medium' | 'large'
  cornerRadius?: number
  requestAccess?: 'write'
}>()

const emit = defineEmits<{
  auth: [data: TelegramAuthData]
}>()

const widgetRef = ref<HTMLDivElement>()

onMounted(() => {
  // Create global callback
  const callbackName = `onTelegramAuth_${Math.random().toString(36).slice(2)}`
  ;(window as unknown as Record<string, unknown>)[callbackName] = (user: TelegramAuthData) => {
    emit('auth', user)
  }

  // Load Telegram widget script
  const script = document.createElement('script')
  script.src = 'https://telegram.org/js/telegram-widget.js?22'
  script.async = true
  script.setAttribute('data-telegram-login', props.botName)
  script.setAttribute('data-size', props.size ?? 'large')
  script.setAttribute('data-onauth', `${callbackName}(user)`)
  script.setAttribute('data-request-access', props.requestAccess ?? 'write')
  if (props.cornerRadius !== undefined) {
    script.setAttribute('data-radius', String(props.cornerRadius))
  }

  widgetRef.value?.appendChild(script)
})
</script>

<template>
  <div ref="widgetRef" class="telegram-login-widget"></div>
</template>

<style scoped>
.telegram-login-widget {
  display: flex;
  justify-content: center;
}
</style>
