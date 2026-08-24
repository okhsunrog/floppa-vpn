import { describe, expect, test } from 'vite-plus/test'
import { renderLinks } from './renderLinks'

describe('renderLinks', () => {
  test('renders a markdown link as an anchor', () => {
    expect(renderLinks('see [the docs](https://example.com/x)')).toBe(
      'see <a href="https://example.com/x" class="underline text-[var(--ui-primary)] hover:opacity-80">the docs</a>',
    )
  })

  test('escapes markup in the surrounding text', () => {
    expect(renderLinks('a <b>bold</b> claim')).toBe('a &lt;b&gt;bold&lt;/b&gt; claim')
  })

  test('escapes markup in the label', () => {
    expect(renderLinks('[<img src=x onerror=alert(1)>](https://example.com)')).toContain(
      '&lt;img src=x onerror=alert(1)&gt;',
    )
  })

  test('cannot be broken out of the href attribute', () => {
    // The whole reason this function exists rather than a one-line `replace`.
    const out = renderLinks('[x](https://e.com" onmouseover="alert(1))')
    expect(out).not.toContain('onmouseover="alert(1)"')
    expect(out).toContain('&quot;')
  })

  test('leaves a non-http scheme as plain text', () => {
    expect(renderLinks('[click](javascript:alert(1)')).not.toContain('<a')
    expect(renderLinks('[click](data:text/html,x)')).toBe('[click](data:text/html,x)')
  })

  test('handles several links and keeps the text between them', () => {
    expect(renderLinks('[a](https://a.co) and [b](https://b.co)')).toBe(
      '<a href="https://a.co" class="underline text-[var(--ui-primary)] hover:opacity-80">a</a>' +
        ' and ' +
        '<a href="https://b.co" class="underline text-[var(--ui-primary)] hover:opacity-80">b</a>',
    )
  })

  test('escapes an ampersand in a query string exactly once', () => {
    expect(renderLinks('[x](https://e.com/?a=1&b=2)')).toContain(
      'href="https://e.com/?a=1&amp;b=2"',
    )
  })

  test('passes plain text through unchanged', () => {
    expect(renderLinks('nothing to see')).toBe('nothing to see')
  })
})
