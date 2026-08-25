import { describe, expect, it } from 'vite-plus/test'
import type { AppInfo } from '../bindings'
import { filterApps } from './appFilter'

const app = (label: string, pkg: string, isSystem = false): AppInfo => ({
  label,
  package_name: pkg,
  is_system: isSystem,
  icon: null,
})

const apps: readonly AppInfo[] = [
  app('Zed Editor', 'dev.zed'),
  app('Android System', 'android', true),
  app('Firefox', 'org.mozilla.firefox'),
  app('Chrome', 'com.android.chrome'),
]

describe('filterApps', () => {
  it('hides system apps unless asked, and sorts alphabetically', () => {
    expect(
      filterApps(apps, { query: '', showSystem: false, selected: [] }).map((a) => a.label),
    ).toEqual(['Chrome', 'Firefox', 'Zed Editor'])
    expect(
      filterApps(apps, { query: '', showSystem: true, selected: [] }).map((a) => a.label),
    ).toEqual(['Android System', 'Chrome', 'Firefox', 'Zed Editor'])
  })

  it('floats selected apps to the top', () => {
    const names = filterApps(apps, { query: '', showSystem: false, selected: ['dev.zed'] }).map(
      (a) => a.label,
    )
    expect(names).toEqual(['Zed Editor', 'Chrome', 'Firefox'])
  })

  it('matches the label ignoring case and whitespace, or the package name', () => {
    const byLabel = filterApps(apps, { query: 'ZEDedit', showSystem: false, selected: [] })
    expect(byLabel.map((a) => a.package_name)).toEqual(['dev.zed'])

    const byPackage = filterApps(apps, { query: 'mozilla', showSystem: false, selected: [] })
    expect(byPackage.map((a) => a.label)).toEqual(['Firefox'])
  })

  it('does not reorder the input', () => {
    const input = [...apps]
    filterApps(input, { query: '', showSystem: true, selected: ['org.mozilla.firefox'] })
    expect(input).toEqual(apps)
  })
})
