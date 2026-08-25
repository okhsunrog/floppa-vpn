import type { AppInfo } from '../bindings'

export interface AppFilter {
  /** Matched against the label (whitespace ignored) and the package name, case-insensitively. */
  query: string
  showSystem: boolean
  /** Package names to float to the top of the list. */
  selected: readonly string[]
}

/** The split-tunneling app list: filtered by the search and the system toggle, selected first. */
export function filterApps(apps: readonly AppInfo[], filter: AppFilter): AppInfo[] {
  let list = filter.showSystem ? [...apps] : apps.filter((a) => !a.is_system)

  if (filter.query) {
    const q = filter.query.toLowerCase().replace(/\s+/g, '')
    list = list.filter(
      (a) =>
        a.label.toLowerCase().replace(/\s+/g, '').includes(q) ||
        a.package_name.toLowerCase().includes(q),
    )
  }

  // Selected apps first, then alphabetical
  return list.sort((a, b) => {
    const aSelected = filter.selected.includes(a.package_name)
    const bSelected = filter.selected.includes(b.package_name)
    if (aSelected !== bSelected) return aSelected ? -1 : 1
    return a.label.localeCompare(b.label)
  })
}
