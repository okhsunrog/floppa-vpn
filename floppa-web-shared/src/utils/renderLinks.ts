const HTML_ESCAPES: Record<string, string> = {
  '&': '&amp;',
  '<': '&lt;',
  '>': '&gt;',
  '"': '&quot;',
  "'": '&#39;',
}

function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => HTML_ESCAPES[c]!)
}

const MARKDOWN_LINK = /\[([^\]]+)\]\(([^)]+)\)/g

const LINK_CLASS = 'underline text-[var(--ui-primary)] hover:opacity-80'

/**
 * Render `[label](url)` as an anchor and everything else as text, for `v-html`.
 *
 * Escaping is the point. The previous version substituted straight into
 * `<a href="$2">$1</a>` with no escaping at all, so a `"` anywhere in the URL closed the
 * attribute and everything after it became markup — `[x](" onmouseover="…)` was enough. The
 * changelog it renders comes from our own repository and our own server, so nobody could reach
 * it without already owning one of the two; that is a reason it was never exploited, not a
 * reason for it to be safe.
 *
 * Only `http` and `https` produce a link. Anything else is left as the literal text the author
 * wrote, which is visible and harmless, rather than becoming an anchor with a scheme the
 * surrounding click handler would hand to the system.
 */
export function renderLinks(text: string): string {
  let out = ''
  let cursor = 0

  for (const match of text.matchAll(MARKDOWN_LINK)) {
    const [whole, label, href] = match
    const start = match.index

    out += escapeHtml(text.slice(cursor, start))
    out += /^https?:\/\//i.test(href!)
      ? `<a href="${escapeHtml(href!)}" class="${LINK_CLASS}">${escapeHtml(label!)}</a>`
      : escapeHtml(whole)
    cursor = start + whole.length
  }

  return out + escapeHtml(text.slice(cursor))
}
