import { computed, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { commands, events, type Phase, type TrayView } from '../bindings'
import { useVpnStore } from '../stores/vpnStore'
import { platform } from '@tauri-apps/plugin-os'

/** A `Record` rather than an interpolated key, so a phase added in Rust fails to compile here. */
const STATUS_KEYS: Record<Phase, string> = {
  unknown: 'status.unknown',
  connected: 'status.connected',
  connecting: 'status.connecting',
  verifying_connection: 'status.verifyingConnection',
  disconnected: 'status.disconnected',
  disconnecting: 'status.disconnecting',
  retrying: 'status.retrying',
}

/**
 * The tray's words, and the one thing it can do.
 *
 * The rows themselves are built in Rust and never rebuilt (`src-tauri/src/tray.rs` says why);
 * what travels from here is their text. That keeps the translations in the locale files, where
 * every other string is, instead of in a second copy kept in Rust and left to drift out of step
 * with the language the user actually picked.
 *
 * Mounted at app scope, because a tray click has to work when nothing else is on screen — the
 * window may be hidden, and the route behind it is whatever it happened to be.
 */
export function useTray(): void {
  const { t } = useI18n()
  const vpn = useVpnStore()

  // Android has no tray and no window to close; the commands answer with nothing there, and this
  // saves the IPC that would say nothing.
  if (platform() === 'android') return

  /**
   * The toggle, worded exactly as the card's own button words it.
   *
   * Same branches, same order: cancel while an attempt can be cancelled, disconnect while a
   * tunnel is up, and connect only when there is a config to connect with. A tray that offered
   * "Connect" to a device with no config would be offering an error message.
   */
  const toggle = computed(() => {
    if (vpn.isCancellable) return { label: t('vpn.cancel'), enabled: true }
    if (vpn.isConnected) return { label: t('vpn.disconnect'), enabled: true }
    if (vpn.isBusy) {
      const label = vpn.phase === 'unknown' ? t('status.unknown') : t('vpn.disconnecting')
      return { label, enabled: false }
    }
    return { label: t('vpn.connect'), enabled: vpn.hasConfig }
  })

  const view = computed<TrayView>(() => ({
    // Windows only; Linux tray implementations have no tooltip. Worth carrying anyway: it is the
    // only place the status shows without opening anything.
    tooltip: `Floppa VPN — ${t(STATUS_KEYS[vpn.phase])}`,
    show: t('tray.show'),
    toggle: toggle.value,
    quit: t('tray.quit'),
  }))

  // On the view, not on the tunnel state: the state changes every second while connected — every
  // traffic sample is a new snapshot — and the words change a handful of times per session.
  watch(
    view,
    async (next) => {
      const result = await commands.updateTray(next)
      // A desktop with no status area is a normal thing to be running on, and the app works
      // without one. Reported once per change rather than retried: nothing here can fix it.
      if (result.status === 'error') console.warn('[tray] could not update the tray:', result.error)
    },
    { immediate: true, deep: true },
  )

  void events.trayToggleRequested.listen(() => {
    void vpn.toggle()
  })
}
