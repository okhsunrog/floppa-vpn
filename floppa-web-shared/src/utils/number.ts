/**
 * Parses what an `<input type="number">` hands back (`''` when cleared, a string while typing,
 * a number via `.number`) into a whole number, or null when empty or not an integer. Use this on
 * `update:model-value` instead of `v-model.number`, which stores `''` for a cleared field and
 * would send that to the API.
 */
export function toIntOrNull(v: string | number | null | undefined): number | null {
  if (v === '' || v == null) return null
  const n = typeof v === 'number' ? v : Number(v)
  return Number.isInteger(n) ? n : null
}
