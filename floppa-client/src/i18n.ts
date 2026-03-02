import { createI18n } from 'vue-i18n'
import { sharedLocaleEn, sharedLocaleRu } from 'floppa-web-shared'

function getDefaultLocale(): string {
  const saved = localStorage.getItem('locale')
  if (saved && ['en', 'ru'].includes(saved)) return saved
  const browserLang = navigator.language.slice(0, 2)
  return browserLang === 'ru' ? 'ru' : 'en'
}

export const i18n = createI18n({
  legacy: false,
  locale: getDefaultLocale(),
  fallbackLocale: 'en',
  messages: { en: sharedLocaleEn, ru: sharedLocaleRu },
})
