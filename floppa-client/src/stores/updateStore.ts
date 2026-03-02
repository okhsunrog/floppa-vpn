import { defineStore } from 'pinia'
import { ref } from 'vue'

const GITHUB_RELEASES_URL = 'https://api.github.com/repos/okhsunrog/floppa-vpn/releases/latest'

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
  // Detected at build time isn't possible, but we can check user agent
  if (navigator.userAgent.includes('Android')) return 'android-arm64.apk'
  if (navigator.userAgent.includes('Linux')) return 'linux-x86_64.AppImage'
  if (navigator.userAgent.includes('Windows')) return 'windows-x86_64.exe'
  return ''
}

export const useUpdateStore = defineStore('update', () => {
  const updateInfo = ref<UpdateInfo | null>(null)
  const dismissed = ref(false)
  const forceUpdate = ref<ForceUpdateInfo | null>(null)

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
    } catch {
      // Silently ignore — update check is best-effort
    }
  }

  function setForceUpdate(info: ForceUpdateInfo) {
    forceUpdate.value = info
  }

  function dismiss() {
    dismissed.value = true
  }

  return { updateInfo, dismissed, forceUpdate, checkForUpdates, setForceUpdate, dismiss }
})
