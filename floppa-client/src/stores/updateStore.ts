import { defineStore } from 'pinia'
import { ref } from 'vue'
import bundledChangelog from '../changelog.json'

const GITHUB_REPO = 'okhsunrog/floppa-vpn'
const GITHUB_RELEASES_URL = `https://api.github.com/repos/${GITHUB_REPO}/releases/latest`
const LAST_SEEN_VERSION_KEY = 'lastSeenVersion'

export interface UpdateInfo {
  version: string
  currentVersion: string
  downloadUrl: string
  releaseUrl: string
  publishedAt: string
  body: string | null
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

export interface ChangelogData {
  version: string
  sections: ChangelogSection[]
}

interface GitHubAsset {
  name: string
  browser_download_url: string
}

interface GitHubRelease {
  tag_name: string
  html_url: string
  published_at: string
  body: string | null
  assets: GitHubAsset[]
}

function compareSemver(a: string, b: string): number {
  const pa = a.split('.').map(Number)
  const pb = b.split('.').map(Number)
  for (let i = 0; i < 3; i++) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0)
    if (diff !== 0) return diff
  }
  return 0
}

function getPlatformAssetSuffix(): string {
  if (navigator.userAgent.includes('Android')) return 'android-arm64.apk'
  if (navigator.userAgent.includes('Linux')) return 'linux-x86_64.AppImage'
  if (navigator.userAgent.includes('Windows')) return 'windows-x86_64.exe'
  return ''
}

const changelogCache = new Map<string, ChangelogData>()

async function fetchChangelogForVersion(version: string): Promise<ChangelogData | null> {
  if (changelogCache.has(version)) return changelogCache.get(version)!
  try {
    // Fetch directly from raw GitHub content — release download URLs
    // redirect to objects.githubusercontent.com which fails in Android WebView
    const url = `https://raw.githubusercontent.com/${GITHUB_REPO}/v${version}/floppa-client/src/changelog.json`
    const res = await fetch(url)
    if (!res.ok) return null
    const data: ChangelogData = await res.json()
    changelogCache.set(data.version, data)
    return data
  } catch {
    return null
  }
}

export const useUpdateStore = defineStore('update', () => {
  const updateInfo = ref<UpdateInfo | null>(null)
  const dismissed = ref(false)
  const forceUpdate = ref<ForceUpdateInfo | null>(null)

  const changelog = ref<ChangelogData | null>(null)
  const changelogLoading = ref(false)
  const changelogModalOpen = ref(false)
  const changelogMode = ref<'update' | 'current'>('update')

  async function checkForUpdates() {
    try {
      const response = await fetch(GITHUB_RELEASES_URL, {
        headers: { Accept: 'application/vnd.github+json' },
      })
      if (!response.ok) return

      const release: GitHubRelease = await response.json()
      const remoteVersion = release.tag_name.replace(/^v/, '')
      const currentVersion = __APP_VERSION__

      if (compareSemver(remoteVersion, currentVersion) <= 0) return

      const suffix = getPlatformAssetSuffix()
      const asset = release.assets.find((a) => a.name.endsWith(suffix))

      updateInfo.value = {
        version: remoteVersion,
        currentVersion,
        downloadUrl: asset?.browser_download_url ?? release.html_url,
        releaseUrl: release.html_url,
        publishedAt: release.published_at,
        body: release.body,
      }

      // Pre-fetch changelog for the available update
      fetchChangelogForVersion(remoteVersion)
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
      changelog.value = await fetchChangelogForVersion(updateInfo.value.version)
    } finally {
      changelogLoading.value = false
    }
  }

  function openChangelogForCurrent() {
    changelogMode.value = 'current'
    changelog.value = bundledChangelog as ChangelogData
    changelogModalOpen.value = true
  }

  function checkPostUpdateChangelog() {
    const lastSeen = localStorage.getItem(LAST_SEEN_VERSION_KEY)
    const current = __APP_VERSION__

    // No lastSeenVersion — first install or update from version without this feature.
    // Show changelog either way so users see what's new.
    if (!lastSeen) {
      localStorage.setItem(LAST_SEEN_VERSION_KEY, current)
      changelog.value = bundledChangelog as ChangelogData
      changelogMode.value = 'current'
      changelogModalOpen.value = true
      return
    }

    // No version change
    if (compareSemver(current, lastSeen) <= 0) return

    // Version bumped — show bundled changelog
    localStorage.setItem(LAST_SEEN_VERSION_KEY, current)
    changelog.value = bundledChangelog as ChangelogData
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
    checkForUpdates,
    openChangelogForUpdate,
    openChangelogForCurrent,
    checkPostUpdateChangelog,
    setForceUpdate,
    dismiss,
  }
})
