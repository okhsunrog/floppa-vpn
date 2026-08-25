/**
 * Icon inventory for Nuxt UI's offline client bundle.
 *
 * Outside a Nuxt build, Nuxt UI's `UIcon` renders through `@iconify/vue`, which fetches every
 * icon it does not already have from the Iconify API (api.iconify.design) at runtime. For a VPN
 * client that means empty buttons on a cold start, no icons at all under an always-on lockdown,
 * and a third party that sees which screens the user opens. Nuxt UI's Vite plugin can bundle
 * icons instead (`icon.clientBundle`); both apps feed it the list below, collected from the
 * `i-lucide-<name>` literals in their own sources and in floppa-web-shared, and Nuxt UI adds
 * its components' own defaults. A name that no collection resolves fails the production build,
 * so a typo cannot ship as a blank glyph.
 *
 * Only literals are collected: build an icon name dynamically and it will not be bundled.
 */
import { readFileSync, readdirSync, statSync } from 'node:fs'
import { extname, join } from 'node:path'

const ICON_RE = /\bi-lucide-([a-z0-9]+(?:-[a-z0-9]+)*)\b/g
const SCAN_EXT = new Set(['.vue', '.ts', '.mts', '.js', '.mjs'])
const SKIP_DIRS = new Set(['node_modules', 'dist'])

/** Every `i-lucide-<name>` literal under `roots`, as full `i-lucide-<name>` ids, sorted. */
export function collectLucideIcons(roots: readonly string[]): string[] {
  const names = new Set<string>()
  const visit = (path: string) => {
    const stat = statSync(path, { throwIfNoEntry: false })
    if (!stat) return
    if (stat.isDirectory()) {
      for (const entry of readdirSync(path)) {
        if (!SKIP_DIRS.has(entry)) visit(join(path, entry))
      }
      return
    }
    if (!SCAN_EXT.has(extname(path))) return
    for (const match of readFileSync(path, 'utf8').matchAll(ICON_RE)) {
      names.add(`i-lucide-${match[1]}`)
    }
  }
  for (const root of roots) visit(root)
  return [...names].sort()
}
