import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

export type AppLocale = 'en' | 'ru'

export const APP_LOCALES: readonly { value: AppLocale; label: string }[] = [
  { value: 'en', label: 'English' },
  { value: 'ru', label: 'Русский' },
]

/** localStorage key both apps' `i18n.ts` read at startup to pick the initial locale. */
export const LOCALE_STORAGE_KEY = 'locale'

/**
 * The UI-side locale switch: sets the active vue-i18n locale and persists it under the key the
 * app reads on the next start. Shared by the navbar (button + segmented list) and the public
 * landing (button), so the two never disagree on the storage key or the toggle order.
 */
export function useLocaleSwitch() {
  const { locale } = useI18n()

  const otherLocale = computed<AppLocale>(() => (locale.value === 'ru' ? 'en' : 'ru'))

  function setLocale(next: AppLocale) {
    locale.value = next
    localStorage.setItem(LOCALE_STORAGE_KEY, next)
  }

  function toggleLocale() {
    setLocale(otherLocale.value)
  }

  /** Label for the one-button toggle: the language it switches *to*. */
  const toggleLabel = computed(() => otherLocale.value.toUpperCase())

  return { locale, locales: APP_LOCALES, setLocale, toggleLocale, toggleLabel }
}
