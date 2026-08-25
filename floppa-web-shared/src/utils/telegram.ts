/**
 * Telegram Mini App detection and the bits of `window.Telegram.WebApp` we use. Everything is
 * read lazily and defensively: the global is injected by Telegram's script only inside a Mini
 * App, and a broken injection must never take the app down.
 */

interface TelegramWebApp {
  initData?: string
  ready?: () => void
  expand?: () => void
}

function webApp(): TelegramWebApp | undefined {
  try {
    return (window as { Telegram?: { WebApp?: TelegramWebApp } }).Telegram?.WebApp
  } catch {
    return undefined
  }
}

/** Raw, signed Mini App `initData`, or null when not running inside a Telegram Mini App. */
export function getTelegramInitData(): string | null {
  const initData = webApp()?.initData
  return initData && initData.length > 0 ? initData : null
}

/** True when the page is open inside a Telegram Mini App. */
export function isMiniApp(): boolean {
  return getTelegramInitData() !== null
}

/** The Telegram user id carried in the Mini App `initData` (URL-encoded `user` JSON), if any. */
export function getTelegramUserId(): number | null {
  const initData = getTelegramInitData()
  if (!initData) return null
  try {
    const userJson = new URLSearchParams(initData).get('user')
    if (!userJson) return null
    const id: unknown = JSON.parse(userJson).id
    return typeof id === 'number' ? id : null
  } catch {
    return null
  }
}

/** Tell Telegram the Mini App has rendered and may take the full viewport. No-op elsewhere. */
export function signalMiniAppReady(): void {
  try {
    const app = webApp()
    app?.ready?.()
    app?.expand?.()
  } catch {
    /* ignore — cosmetic */
  }
}
