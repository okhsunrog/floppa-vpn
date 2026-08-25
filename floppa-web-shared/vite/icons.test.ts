import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { getIconData } from '@iconify/utils'
import { describe, expect, it } from 'vite-plus/test'
import { collectLucideIcons } from './icons'

const here = resolve(new URL('.', import.meta.url).pathname)
const APP_ROOTS = [
  resolve(here, '../src'),
  resolve(here, '../../floppa-client/src'),
  resolve(here, '../../floppa-face/src'),
]

describe('collectLucideIcons', () => {
  it('finds i-lucide literals in vue and ts files, ignores other collections and dirs', () => {
    const dir = mkdtempSync(join(tmpdir(), 'icons-'))
    mkdirSync(join(dir, 'node_modules', 'x'), { recursive: true })
    writeFileSync(join(dir, 'a.vue'), '<UIcon name="i-lucide-check" /> i-lucide-arrow-up-right')
    writeFileSync(join(dir, 'b.ts'), "const x = cond ? 'i-lucide-globe' : 'i-simple-icons-github'")
    writeFileSync(join(dir, 'c.md'), 'i-lucide-not-scanned')
    writeFileSync(join(dir, 'node_modules', 'x', 'd.ts'), "'i-lucide-skipped'")
    expect(collectLucideIcons([dir])).toEqual([
      'i-lucide-arrow-up-right',
      'i-lucide-check',
      'i-lucide-globe',
    ])
  })
})

describe('the icons the apps use', () => {
  it('all exist in @iconify-json/lucide (a typo here would ship as a blank glyph)', () => {
    const require = createRequire(import.meta.url)
    const lucide = JSON.parse(
      readFileSync(require.resolve('@iconify-json/lucide/icons.json'), 'utf8'),
    )
    const icons = collectLucideIcons(APP_ROOTS)
    expect(icons.length).toBeGreaterThan(50)
    const missing = icons.filter((icon) => !getIconData(lucide, icon.replace(/^i-lucide-/, '')))
    expect(missing).toEqual([])
  })
})
