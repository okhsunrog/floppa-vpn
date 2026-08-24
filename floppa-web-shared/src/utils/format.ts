/**
 * Format bytes to human readable string (e.g., "1.5 GB")
 */
export function formatBytes(bytes: number, decimals = 2): string {
  if (bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']
  // Clamp so huge (>= 1 EB) or fractional (< 1 B) inputs never index out of `sizes` ("1 undefined").
  const i = Math.min(Math.max(Math.floor(Math.log(bytes) / Math.log(k)), 0), sizes.length - 1)
  return parseFloat((bytes / Math.pow(k, i)).toFixed(decimals)) + ' ' + sizes[i]
}

/**
 * Format bytes per second to human readable speed string (e.g., "1.5 MB/s")
 */
export function formatSpeed(bytesPerSecond: number, decimals = 1): string {
  if (bytesPerSecond === 0) return '0 B/s'
  const k = 1024
  const sizes = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  // Clamp so >= 1 TB/s (or fractional) inputs never index out of `sizes` ("… undefined").
  const i = Math.min(
    Math.max(Math.floor(Math.log(bytesPerSecond) / Math.log(k)), 0),
    sizes.length - 1,
  )
  return parseFloat((bytesPerSecond / Math.pow(k, i)).toFixed(decimals)) + ' ' + sizes[i]
}

/**
 * Format date to locale string
 */
export function formatDate(date: string | Date): string {
  const d = typeof date === 'string' ? new Date(date) : date
  return d.toLocaleDateString()
}

/**
 * Format date to locale date and time string
 */
export function formatDateTime(date: string | Date): string {
  const d = typeof date === 'string' ? new Date(date) : date
  return d.toLocaleString()
}

/**
 * Format duration in seconds to human readable string (e.g., "2h 30m")
 */
export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ${minutes % 60}m`
  const days = Math.floor(hours / 24)
  return `${days}d ${hours % 24}h`
}

/**
 * Split a minute count into the largest whole unit, for localized display of plan
 * trial durations (e.g. 10080 -> {unit:'days', n:7}, 120 -> {unit:'hours', n:2},
 * 90 -> {unit:'minutes', n:90}). The caller renders it via the matching i18n key.
 */
export function durationUnit(minutes: number): { unit: 'days' | 'hours' | 'minutes'; n: number } {
  if (minutes % 1440 === 0) return { unit: 'days', n: minutes / 1440 }
  if (minutes % 60 === 0) return { unit: 'hours', n: minutes / 60 }
  return { unit: 'minutes', n: minutes }
}

/**
 * Format speed limit (null = unlimited)
 */
export function formatSpeedLimit(
  mbps: number | null | undefined,
  unlimitedLabel = 'Unlimited',
): string {
  return mbps == null ? unlimitedLabel : `${mbps} Mbps`
}
