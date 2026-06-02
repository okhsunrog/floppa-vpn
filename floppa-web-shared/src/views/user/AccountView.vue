<script setup lang="ts">
import { ref, computed, onUnmounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useQuery } from '@pinia/colada'
import { getMeQuery } from '../../client/@pinia/colada.gen'
import { setMyCredential, startTelegramLink, pollTelegramLink } from '../../client/sdk.gen'
import { openExternal } from '../../utils/openExternal'

const { t } = useI18n()
const { data: me, refetch: refetchMe } = useQuery(getMeQuery())

const telegramLinked = computed(() => !!me.value?.telegram_linked)
const hasCredential = computed(() => !!me.value?.has_credential)

// ── Backup credential (login + password) ──
const login = ref('')
const password = ref('')
const credLoading = ref(false)
const credError = ref<string | null>(null)
const credSuccess = ref(false)

async function saveCredential() {
  const value = login.value.trim()
  if (!value || !password.value) return
  credError.value = null
  credSuccess.value = false
  credLoading.value = true
  try {
    await setMyCredential({ body: { login: value, password: password.value }, throwOnError: true })
    credSuccess.value = true
    password.value = ''
    await refetchMe()
  } catch (e) {
    const status = (e as { status?: number })?.status
    credError.value = status === 409 ? t('account.loginTaken') : t('account.credentialError')
  } finally {
    credLoading.value = false
  }
}

// ── Link Telegram (open deep link, then poll) ──
const linking = ref(false)
const linkError = ref<string | null>(null)
let pollTimer: ReturnType<typeof setInterval> | null = null

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer)
    pollTimer = null
  }
}
onUnmounted(stopPolling)

async function linkTelegram() {
  linkError.value = null
  linking.value = true
  try {
    const { data } = await startTelegramLink({ throwOnError: true })
    await openExternal(data.deep_link)

    // Poll for completion (cap ~5 min).
    let elapsed = 0
    stopPolling()
    pollTimer = setInterval(async () => {
      elapsed += 2
      try {
        const { data: poll } = await pollTelegramLink({ throwOnError: true })
        if (poll.linked) {
          stopPolling()
          linking.value = false
          await refetchMe()
        }
      } catch {
        /* transient — keep polling */
      }
      if (elapsed >= 300) {
        stopPolling()
        linking.value = false
      }
    }, 2000)
  } catch (e) {
    const status = (e as { status?: number })?.status
    linkError.value = status === 409 ? t('account.alreadyLinked') : t('account.linkError')
    linking.value = false
  }
}
</script>

<template>
  <div class="max-w-3xl mx-auto">
    <h1 class="hidden md:block text-2xl font-bold mb-6">{{ t('account.title') }}</h1>

    <!-- Login methods -->
    <UCard class="mb-6">
      <template #header>
        <div class="flex items-center gap-2 font-semibold">
          <UIcon name="i-lucide-key-round" />
          {{ t('account.loginMethods') }}
        </div>
      </template>

      <div class="flex flex-col gap-4">
        <!-- Telegram -->
        <div class="flex items-center justify-between gap-3">
          <div class="flex items-center gap-2">
            <UIcon name="i-lucide-send" class="size-5" />
            <span>Telegram</span>
          </div>
          <UBadge v-if="telegramLinked" color="success" :label="t('account.linked')" />
          <UButton v-else size="sm" :loading="linking" @click="linkTelegram">
            {{ t('account.linkTelegram') }}
          </UButton>
        </div>
        <p v-if="linking" class="text-xs text-[var(--ui-text-muted)]">
          {{ t('account.linkWaiting') }}
        </p>
        <UAlert v-if="linkError" color="error" :title="linkError" />

        <!-- Login + password -->
        <div
          class="flex items-center justify-between gap-3 border-t border-[var(--ui-border)] pt-4"
        >
          <div class="flex items-center gap-2">
            <UIcon name="i-lucide-lock" class="size-5" />
            <span>{{ t('account.loginPassword') }}</span>
          </div>
          <UBadge v-if="hasCredential" color="success" :label="t('account.set')" />
          <UBadge v-else color="neutral" variant="subtle" :label="t('account.notSet')" />
        </div>
      </div>
    </UCard>

    <!-- Set / change backup credential -->
    <UCard>
      <template #header>
        <div class="flex items-center gap-2 font-semibold">
          <UIcon name="i-lucide-shield-check" />
          {{ hasCredential ? t('account.changeCredential') : t('account.setCredential') }}
        </div>
      </template>

      <p class="text-sm text-[var(--ui-text-muted)] mb-4">{{ t('account.credentialHint') }}</p>
      <div class="flex flex-col gap-3 max-w-sm">
        <UInput
          v-model="login"
          :placeholder="t('account.loginPlaceholder')"
          icon="i-lucide-user"
          autocomplete="username"
        />
        <UInput
          v-model="password"
          type="password"
          :placeholder="t('account.passwordPlaceholder')"
          icon="i-lucide-lock"
          autocomplete="new-password"
          @keydown.enter="saveCredential"
        />
        <UButton
          color="primary"
          :loading="credLoading"
          :disabled="!login.trim() || !password"
          @click="saveCredential"
        >
          {{ t('account.save') }}
        </UButton>
        <UAlert v-if="credError" color="error" :title="credError" />
        <UAlert v-if="credSuccess" color="success" :title="t('account.credentialSaved')" />
      </div>
    </UCard>
  </div>
</template>
