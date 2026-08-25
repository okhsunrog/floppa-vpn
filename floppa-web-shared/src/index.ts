// Components
export * from './components'

// Utilities
export * from './utils'

// Types
export * from './types'

// Stores
export * from './stores'

// Views
export * from './views'

// Router
export * from './router'

// App wiring shared by the admin panel and the client
export * from './api/interceptors'
export { createSharedI18n, type SharedLocale } from './i18n'

// Locales
export { default as sharedLocaleEn } from './locales/en'
export { default as sharedLocaleRu } from './locales/ru'
