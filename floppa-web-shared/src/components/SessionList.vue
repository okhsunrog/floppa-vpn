<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type { SessionInfo } from '../client/types.gen'
import { formatDateTime, formatRelativeTime } from '../utils/format'
import {
  SESSION_KIND_COLORS,
  SESSION_KIND_LABEL_KEYS,
  sessionIcon,
  sessionTitle,
} from '../composables/sessions'

/**
 * The rows of a "Devices & sessions" list, shared by the account page (the user's own sessions,
 * one marked as this device) and the admin user detail (someone else's, none current). The
 * parent owns the data and the confirm/revoke flow; this only renders and emits.
 */
defineProps<{
  sessions: SessionInfo[]
  /** Id of the session a sign-out is in flight for, to spin that row's button. */
  revokingId?: string | null
}>()

const emit = defineEmits<{ revoke: [id: string] }>()

const { t, locale } = useI18n()
</script>

<template>
  <ul class="flex flex-col divide-y divide-[var(--ui-border)]">
    <li
      v-for="session in sessions"
      :key="session.id"
      class="flex items-center justify-between gap-3 py-3 first:pt-0 last:pb-0"
    >
      <div class="flex items-center gap-3 min-w-0">
        <UIcon :name="sessionIcon(session)" class="size-5 shrink-0 text-[var(--ui-text-muted)]" />
        <div class="flex flex-col gap-0.5 min-w-0">
          <div class="flex items-center gap-2 flex-wrap">
            <span class="font-medium truncate">
              {{ sessionTitle(session, t('sessions.unnamedDevice')) }}
            </span>
            <UBadge
              v-if="session.current"
              color="primary"
              variant="subtle"
              size="sm"
              :label="t('sessions.thisDevice')"
            />
            <UBadge
              :color="SESSION_KIND_COLORS[session.kind]"
              variant="subtle"
              size="sm"
              :label="t(SESSION_KIND_LABEL_KEYS[session.kind])"
            />
          </div>
          <span
            class="text-xs text-[var(--ui-text-muted)]"
            :title="formatDateTime(session.last_seen_at)"
          >
            {{ t('sessions.lastSeen', { when: formatRelativeTime(session.last_seen_at, locale) }) }}
            &middot;
            {{ t('sessions.signedIn', { date: formatDateTime(session.created_at) }) }}
          </span>
        </div>
      </div>
      <UButton
        :label="t('sessions.signOut')"
        icon="i-lucide-log-out"
        color="error"
        variant="outline"
        size="sm"
        :loading="revokingId === session.id"
        @click="emit('revoke', session.id)"
      />
    </li>
  </ul>
</template>
