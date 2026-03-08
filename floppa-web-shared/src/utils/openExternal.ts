const isTauri = Boolean((window as unknown as Record<string, unknown>).__TAURI_INTERNALS__)

/** Open a URL in the system browser / OS handler. Works in Tauri, browser, and TG mini app. */
export async function openExternal(url: string) {
  if (isTauri) {
    const { openUrl } = await import('@tauri-apps/plugin-opener')
    await openUrl(url)
  } else {
    window.open(url, '_blank')
  }
}

/** Click handler for `<a>` tags — intercepts clicks to use system browser in Tauri. */
export function handleExternalLinkClick(event: MouseEvent) {
  const target = (event.target as HTMLElement).closest('a')
  if (target?.href) {
    event.preventDefault()
    openExternal(target.href)
  }
}
