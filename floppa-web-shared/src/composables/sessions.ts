import { ref } from 'vue'
import type { BadgeProps } from '@nuxt/ui'
import type { SessionInfo, SessionKind } from '../client/types.gen'

/**
 * Records over the generated union: a kind added on the server without an entry here is a
 * compile error, not a badge showing the raw enum value.
 */
export const SESSION_KIND_LABEL_KEYS: Record<SessionKind, string> = {
  telegram_widget: 'sessions.kindTelegramWidget',
  mini_app: 'sessions.kindMiniApp',
  deep_link: 'sessions.kindDeepLink',
  credential: 'sessions.kindCredential',
  legacy: 'sessions.kindLegacy',
}

export const SESSION_KIND_COLORS: Record<SessionKind, BadgeProps['color']> = {
  telegram_widget: 'info',
  mini_app: 'info',
  deep_link: 'success',
  credential: 'warning',
  legacy: 'neutral',
}

/** Icon for the device behind a session, from the platform the app reported (if any). */
export function sessionIcon(session: Pick<SessionInfo, 'platform' | 'kind'>): string {
  switch (session.platform?.toLowerCase()) {
    case 'android':
    case 'ios':
      return 'i-lucide-smartphone'
    case 'linux':
    case 'windows':
    case 'macos':
      return 'i-lucide-laptop'
    default:
      return session.kind === 'telegram_widget' || session.kind === 'mini_app'
        ? 'i-lucide-globe'
        : 'i-lucide-monitor-smartphone'
  }
}

/**
 * What to call a session: the label the app stamped when it registered its device, else the
 * device name or platform, else `fallback` (an "unnamed device" message).
 */
export function sessionTitle(
  session: Pick<SessionInfo, 'label' | 'device_name' | 'platform'>,
  fallback: string,
): string {
  return session.label || session.device_name || session.platform || fallback
}

/**
 * Confirmation-modal state for signing out one session. Mirrors `useConfirmAction` from
 * `adminList`, but session ids are UUIDs, not row numbers.
 */
export function useSessionConfirm() {
  const open = ref(false)
  const pendingId = ref<string | null>(null)

  function request(id: string) {
    pendingId.value = id
    open.value = true
  }

  function reset() {
    open.value = false
    pendingId.value = null
  }

  async function confirm(action: (id: string) => Promise<void>) {
    if (pendingId.value === null) return
    await action(pendingId.value)
    reset()
  }

  return { open, pendingId, request, confirm, reset }
}
