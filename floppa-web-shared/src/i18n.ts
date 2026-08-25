import { createI18n } from 'vue-i18n'
import sharedLocaleEn from './locales/en'
import sharedLocaleRu from './locales/ru'

const LOCALE_STORAGE_KEY = 'locale'

export type SharedLocale = 'en' | 'ru'

function isSharedLocale(value: string | null): value is SharedLocale {
  return value === 'en' || value === 'ru'
}

function getDefaultLocale(): SharedLocale {
  const saved = localStorage.getItem(LOCALE_STORAGE_KEY)
  if (isSharedLocale(saved)) return saved
  return navigator.language.slice(0, 2) === 'ru' ? 'ru' : 'en'
}

/**
 * The i18n instance both apps install: the shared locales, the saved or browser-derived
 * starting locale, English as the fallback. One factory rather than one copy per app, so the
 * two can never drift on which locales exist or how the default is chosen.
 */
export function createSharedI18n() {
  return createI18n({
    legacy: false,
    locale: getDefaultLocale(),
    fallbackLocale: 'en',
    messages: { en: sharedLocaleEn, ru: sharedLocaleRu },
  })
}
