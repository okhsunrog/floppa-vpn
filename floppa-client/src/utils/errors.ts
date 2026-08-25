/**
 * Text for a rejected Tauri invoke, which is not a Result: a string, an `Error`, or whatever
 * object the Rust side serialised. Nothing here is an API error — those come typed through
 * `isApiError` in the shared package.
 */
export function describeUnknown(e: unknown): string {
  if (e instanceof Error) return e.message
  if (typeof e === 'string') return e
  try {
    return JSON.stringify(e)
  } catch {
    return 'unknown error'
  }
}
