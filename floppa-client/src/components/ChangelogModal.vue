<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { DialogTitle, DialogDescription, VisuallyHidden } from 'reka-ui'
import { useUpdateStore, type ChangelogItem } from '../stores/updateStore'

const { t, locale } = useI18n()
const updateStore = useUpdateStore()

const sectionIcons: Record<string, string> = {
  added: 'i-lucide-plus-circle',
  fixed: 'i-lucide-wrench',
  changed: 'i-lucide-refresh-cw',
  notes: 'i-lucide-info',
}

const accordionItems = computed(() => {
  if (!updateStore.changelog) return []
  return updateStore.changelog.sections
    .filter((s) => s.items.length > 0)
    .map((section) => ({
      label: t(`changelog.${section.type}`),
      icon: sectionIcons[section.type],
      value: section.type,
      content: ' ', // non-empty string needed for UAccordion to render body slot
      section,
    }))
})

const defaultOpen = computed(() => updateStore.changelog?.sections.map((s) => s.type) ?? [])

function getItemText(item: ChangelogItem): string {
  return locale.value === 'ru' ? item.ru : item.en
}

/** Convert markdown-style [text](url) links to <a> tags */
function renderLinks(text: string): string {
  return text.replace(
    /\[([^\]]+)\]\(([^)]+)\)/g,
    '<a href="$2" class="underline text-[var(--ui-primary)] hover:opacity-80">$1</a>',
  )
}

/** Intercept <a> clicks to open in system browser */
function handleLinkClick(event: MouseEvent) {
  const target = (event.target as HTMLElement).closest('a')
  if (target?.href) {
    event.preventDefault()
    openUrl(target.href)
  }
}

const isUpdateMode = computed(() => updateStore.changelogMode === 'update')
</script>

<template>
  <UModal v-model:open="updateStore.changelogModalOpen">
    <template #header>
      <VisuallyHidden>
        <DialogTitle>{{
          t('changelog.title', { version: updateStore.changelog?.version ?? '' })
        }}</DialogTitle>
        <DialogDescription>{{
          t('changelog.title', { version: updateStore.changelog?.version ?? '' })
        }}</DialogDescription>
      </VisuallyHidden>
      <div class="flex items-center gap-1">
        <UButton
          icon="i-lucide-chevron-left"
          variant="ghost"
          size="xs"
          :disabled="!updateStore.hasNewerChangelog"
          @click="updateStore.changelogNewer()"
        />
        <span class="text-lg font-semibold">{{
          t('changelog.title', { version: updateStore.changelog?.version ?? '' })
        }}</span>
        <UButton
          icon="i-lucide-chevron-right"
          variant="ghost"
          size="xs"
          :disabled="!updateStore.hasOlderChangelog"
          @click="updateStore.changelogOlder()"
        />
      </div>
    </template>
    <template #body>
      <div v-if="updateStore.changelogLoading" class="flex justify-center py-8">
        <div class="animate-spin i-lucide-loader-2 size-6 text-[var(--ui-primary)]" />
      </div>

      <div v-else-if="!updateStore.changelog" class="text-center py-8 text-[var(--ui-text-muted)]">
        {{ t('changelog.notAvailable') }}
      </div>

      <UAccordion v-else :items="accordionItems" type="multiple" :default-value="defaultOpen">
        <template #body="{ item }">
          <!-- eslint-disable-next-line vue/no-v-html -->
          <div class="flex flex-col gap-2" @click="handleLinkClick">
            <div
              v-for="(entry, idx) in item.section.items"
              :key="idx"
              class="rounded-lg bg-[var(--ui-bg-elevated)] px-3 py-2 text-sm"
              v-html="renderLinks(getItemText(entry))"
            />
          </div>
        </template>
      </UAccordion>
    </template>

    <template #footer>
      <div class="flex justify-end gap-2">
        <UButton
          :label="t('common.close')"
          color="primary"
          @click="updateStore.changelogModalOpen = false"
        />
        <UButton
          v-if="isUpdateMode && updateStore.updateInfo"
          :label="t('update.download')"
          @click="openUrl(updateStore.updateInfo!.downloadUrl)"
        />
      </div>
    </template>
  </UModal>
</template>
