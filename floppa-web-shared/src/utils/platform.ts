/** True when running inside the Tauri webview (desktop or Android client), false on the web. */
export function isTauri(): boolean {
  return '__TAURI_INTERNALS__' in window
}
