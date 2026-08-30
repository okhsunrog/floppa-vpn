<script setup lang="ts">
import { watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { animations } from '@formkit/drag-and-drop'
import { useDragAndDrop } from '@formkit/drag-and-drop/vue'
import { useSettingsStore } from '../stores/settingsStore'
import { useVpnStore } from '../stores/vpnStore'
import type { Protocol } from '../bindings'

/**
 * The draggable priority list — in a component of its own so that it is built fresh every time
 * the modal opens, which is the whole reason this file exists.
 *
 * `useDragAndDrop` binds to the list element through a watcher that stops itself the moment it
 * sees one (`handleVueElements` in @formkit/drag-and-drop). A modal unmounts its body on close,
 * so the second open creates a new `<ul>` that nothing is watching any more, and nothing in it is
 * draggable. Measured before the fix: `draggable="true"` on every row on the first open, absent
 * on the second. `setup` runs per mount, so a component boundary is the fix — a new list gets a
 * new watcher — and seeding from the store on each open is right for its own sake.
 */
const { t } = useI18n()
const settings = useSettingsStore()
const vpn = useVpnStore()

/**
 * Local draggable list seeded from the persisted priority; synced back on reorder.
 *
 * The options are all about how the reorder *feels*, which without them is a row that teleports —
 * and, on a phone, one that could hardly be picked up at all:
 *
 * - `animations` slides the rows that move out of the way instead of snapping them. It is the
 *   library's own plugin and the reason a rewrite was not needed.
 * - the classes say what is happening: the row you hold fades and lifts, the gap it would drop
 *   into is outlined. `synth*` are the touch equivalents — a phone does not raise native drag
 *   events, so the library draws its own, with its own class names.
 * - `longPress` on touch, because this list lives inside a scrollable modal: without it, the
 *   first finger-drag over a row grabs the row instead of scrolling the sheet.
 *
 * There is deliberately no `dragHandle`. It was the grip icon, 16 pixels of it, which is a
 * quarter of the smallest thing a finger is expected to hit — with a one-second hold on top, the
 * list was close to unusable on a phone. The whole row is the target now and the grip is what says
 * so.
 */
const [listRef, order] = useDragAndDrop<Protocol>([...settings.protocolOrder], {
  plugins: [animations({ duration: 200 })],
  draggingClass: 'opacity-40',
  dropZoneClass: 'ring-2 ring-[var(--ui-primary)]',
  synthDraggingClass: 'opacity-40 scale-[1.02] shadow-lg',
  synthDropZoneClass: 'ring-2 ring-[var(--ui-primary)]',
  longPress: true,
  // A quarter of a second, against a default of a full one. Long enough that a flick to scroll
  // the sheet is still a scroll, short enough that picking a row up does not feel broken —
  // Android's own long-press is 500ms and that is for a menu, not for something you are already
  // holding on purpose.
  longPressDuration: 250,
  longPressClass: 'ring-2 ring-[var(--ui-primary)]',
})

watch(
  order,
  (value) => {
    settings.protocolOrder = [...value]
  },
  { deep: true },
)
</script>

<template>
  <ul ref="listRef" class="flex flex-col gap-2">
    <li
      v-for="proto in order"
      :key="proto"
      class="flex cursor-grab items-center gap-3 rounded-lg bg-[var(--ui-bg-elevated)] px-3 py-3 active:cursor-grabbing"
    >
      <UIcon name="i-lucide-grip-vertical" class="size-4 shrink-0 text-[var(--ui-text-muted)]" />
      <span class="text-sm font-medium">{{ t(`vpn.${proto}`) }}</span>
      <UBadge
        v-if="!vpn.availableProtocols.includes(proto)"
        color="neutral"
        variant="subtle"
        size="xs"
        class="ml-auto"
      >
        {{ t('settings.protocolUnavailable') }}
      </UBadge>
    </li>
  </ul>
</template>
