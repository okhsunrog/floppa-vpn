<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { commands, events, type Phase } from '../bindings'
import { useSettingsStore } from '../stores/settingsStore'
import { useVpnStore } from '../stores/vpnStore'

/**
 * What the window's close button means, asked once.
 *
 * Rust prevents every close and asks here (`src-tauri/src/tray.rs`), because the answer is a
 * setting and the settings are on this side.
 *
 * Asked on the first close whatever the tunnel is doing. Asking only with a tunnel up was the
 * first attempt — the question costs something only then — and it made the whole feature
 * invisible: close the app while disconnected, which is what anyone does at least once, and it
 * simply quits, with nothing to suggest a tray was ever an option. What the state changes is the
 * wording, not whether the question is put.
 */
const { t } = useI18n()
const settings = useSettingsStore()
const vpn = useVpnStore()

const open = ref(false)
const remember = ref(false)

/**
 * Whether closing would cost the user a tunnel. Decides the wording, never whether we ask.
 *
 * Retrying counts: a tunnel the actor is bringing back is one the user asked for and expects.
 *
 * The phase, and deliberately not the intent — which was the first attempt and was wrong. The
 * actor raises an Up intent of its own at startup, with no params, as the only intent allowed to
 * adopt a tunnel that outlived the last process (`actor::bootstrap`). So `intent` reads `up` on a
 * freshly launched app connected to nothing, and the dialog told everyone their VPN was on.
 */
const TUNNEL_AT_STAKE: Record<Phase, boolean> = {
  connected: true,
  connecting: true,
  verifying_connection: true,
  retrying: true,
  disconnecting: false,
  disconnected: false,
  unknown: false,
}

const vpnOn = computed(() => TUNNEL_AT_STAKE[vpn.state.phase])

onMounted(() => {
  void events.windowCloseRequested.listen(() => {
    // The other half of the line Rust logs when it prevents a close: together they say whether a
    // close that appeared to do nothing was never delivered, or was delivered and acted on.
    console.info(
      `[close] asked what closing means; behavior=${settings.closeBehavior} phase=${vpn.state.phase}`,
    )
    if (settings.closeBehavior === 'tray') return void commands.hideToTray()
    if (settings.closeBehavior === 'quit') return void commands.quitApp()
    remember.value = false
    open.value = true
  })
})

/**
 * How long the dialog needs to animate away before the window may go.
 *
 * Precaution, not a fix for anything observed: a webview whose window is off the screen may stop
 * producing animation frames, and a transition caught mid-flight would then never finish. The
 * wait costs nothing anyone minds — it is the dialog closing, which is what a click on it should
 * look like.
 */
const DIALOG_CLOSE_MS = 300

function choose(behavior: 'tray' | 'quit') {
  if (remember.value) settings.closeBehavior = behavior
  open.value = false
  setTimeout(() => {
    if (behavior === 'tray') void commands.hideToTray()
    else void commands.quitApp()
  }, DIALOG_CLOSE_MS)
}
</script>

<template>
  <UModal v-model:open="open" :title="t('close.title')">
    <template #body>
      <p class="text-sm text-(--ui-text-muted)">
        {{ vpnOn ? t('close.descriptionConnected') : t('close.descriptionIdle') }}
      </p>
      <UCheckbox v-model="remember" :label="t('close.remember')" class="mt-4" />
    </template>
    <template #footer>
      <UButton
        :label="vpnOn ? t('close.quitConnected') : t('close.quitIdle')"
        icon="i-lucide-power-off"
        color="neutral"
        variant="outline"
        @click="choose('quit')"
      />
      <UButton :label="t('close.minimize')" icon="i-lucide-chevron-down" @click="choose('tray')" />
    </template>
  </UModal>
</template>
