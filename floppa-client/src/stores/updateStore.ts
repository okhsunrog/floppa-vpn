import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { platform } from '@tauri-apps/plugin-os'
import bundledChangelog from 'floppa-web-shared/changelog.json'
import { compareSemver } from '../utils/semver'

const LAST_SEEN_VERSION_KEY = 'lastSeenVersion'

// Self-hosted update source: our server mirrors the latest release (binaries + metadata) at
// <origin>/downloads/, so the update check, download and changelog are served from our own
// origin instead of GitHub — insurance in case GitHub becomes unreachable for clients.
const API_URL = (import.meta.env.VITE_API_URL as string) ?? ''
const DOWNLOADS_BASE = API_URL.replace(/\/api\/?$/, '').replace(/\/+$/, '') + '/downloads'

export interface UpdateInfo {
  version: string
  currentVersion: string
  downloadUrl: string
}

export interface ForceUpdateInfo {
  minVersion: string
  message: string
}

export interface ChangelogItem {
  en: string
  ru: string
}

export interface ChangelogSection {
  type: 'added' | 'fixed' | 'changed' | 'notes'
  items: ChangelogItem[]
}

export interface ChangelogEntry {
  version: string
  sections: ChangelogSection[]
}

export interface ChangelogData extends ChangelogEntry {
  history?: ChangelogEntry[]
}

// Shape of /downloads/latest.json, written by the mirror-release workflow.
interface LatestJson {
  version: string
  tag: string
  files: Partial<Record<string, string>>
}

// Map the running platform to its key in /downloads/latest.json `files`.
function platformFileKey(): string | null {
  switch (platform()) {
    case 'android':
      return 'android'
    case 'linux':
      return 'linux_appimage'
    case 'windows':
      return 'windows_exe'
    default:
      return null
  }
}

let changelogCache: ChangelogData | null = null

// The server mirrors the latest published version's changelog at /downloads/changelog.json.
async function fetchLatestChangelog(): Promise<ChangelogData | null> {
  if (changelogCache) return changelogCache
  try {
    const res = await fetch(`${DOWNLOADS_BASE}/changelog.json`, { cache: 'no-cache' })
    if (!res.ok) return null
    changelogCache = (await res.json()) as ChangelogData
    return changelogCache
  } catch {
    return null
  }
}

export const useUpdateStore = defineStore('update', () => {
  const updateInfo = ref<UpdateInfo | null>(null)
  const dismissed = ref(false)
  const forceUpdate = ref<ForceUpdateInfo | null>(null)

  const changelog = ref<ChangelogEntry | null>(null)
  const changelogLoading = ref(false)
  const changelogModalOpen = ref(false)
  const changelogMode = ref<'update' | 'current'>('update')
  const changelogEntries = ref<ChangelogEntry[]>([])
  const changelogIndex = ref(0)

  const hasNewerChangelog = computed(() => changelogIndex.value > 0)
  const hasOlderChangelog = computed(() => changelogIndex.value < changelogEntries.value.length - 1)

  function changelogNewer() {
    if (!hasNewerChangelog.value) return
    changelogIndex.value--
    changelog.value = changelogEntries.value[changelogIndex.value]!
  }

  function changelogOlder() {
    if (!hasOlderChangelog.value) return
    changelogIndex.value++
    changelog.value = changelogEntries.value[changelogIndex.value]!
  }

  function loadChangelogData(data: ChangelogData) {
    const { history, ...current } = data
    changelogEntries.value = [current, ...(history ?? [])]
    changelogIndex.value = 0
    changelog.value = changelogEntries.value[0]!
  }

  async function checkForUpdates() {
    try {
      const res = await fetch(`${DOWNLOADS_BASE}/latest.json`, { cache: 'no-cache' })
      if (!res.ok) return

      const latest = (await res.json()) as LatestJson
      const remoteVersion = latest.version
      const currentVersion = __APP_VERSION__

      // An unreadable version is not an update. Offering one we cannot compare would leave a
      // banner the user can never satisfy.
      const newer = compareSemver(remoteVersion, currentVersion)
      if (newer === null || newer <= 0) return

      const key = platformFileKey()
      const file = key ? latest.files?.[key] : undefined

      updateInfo.value = {
        version: remoteVersion,
        currentVersion,
        downloadUrl: file ? `${DOWNLOADS_BASE}/${file}` : DOWNLOADS_BASE,
      }

      // Pre-fetch changelog for the available update
      void fetchLatestChangelog()
    } catch {
      // Silently ignore — update check is best-effort
    }
  }

  async function openChangelogForUpdate() {
    if (!updateInfo.value) return
    changelogMode.value = 'update'
    changelogLoading.value = true
    changelogModalOpen.value = true
    try {
      const data = await fetchLatestChangelog()
      if (data) {
        loadChangelogData(data)
      } else {
        changelog.value = null
      }
    } finally {
      changelogLoading.value = false
    }
  }

  function openChangelogForCurrent() {
    changelogMode.value = 'current'
    loadChangelogData(bundledChangelog as ChangelogData)
    changelogModalOpen.value = true
  }

  function checkPostUpdateChangelog() {
    const lastSeen = localStorage.getItem(LAST_SEEN_VERSION_KEY)
    const current = __APP_VERSION__

    // No lastSeenVersion — first install or update from version without this feature.
    // Show changelog either way so users see what's new.
    if (!lastSeen) {
      localStorage.setItem(LAST_SEEN_VERSION_KEY, current)
      loadChangelogData(bundledChangelog as ChangelogData)
      changelogMode.value = 'current'
      changelogModalOpen.value = true
      return
    }

    // No version change — or a marker we cannot read, which is not grounds to show anything.
    const bumped = compareSemver(current, lastSeen)
    if (bumped === null || bumped <= 0) return

    // Version bumped — show bundled changelog
    localStorage.setItem(LAST_SEEN_VERSION_KEY, current)
    loadChangelogData(bundledChangelog as ChangelogData)
    changelogMode.value = 'current'
    changelogModalOpen.value = true
  }

  function setForceUpdate(info: ForceUpdateInfo) {
    forceUpdate.value = info
  }

  function dismiss() {
    dismissed.value = true
  }

  return {
    updateInfo,
    dismissed,
    forceUpdate,
    changelog,
    changelogLoading,
    changelogModalOpen,
    changelogMode,
    hasNewerChangelog,
    hasOlderChangelog,
    checkForUpdates,
    openChangelogForUpdate,
    openChangelogForCurrent,
    checkPostUpdateChangelog,
    changelogNewer,
    changelogOlder,
    setForceUpdate,
    dismiss,
  }
})
