import './assets/main.css'

import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { PiniaColada } from '@pinia/colada'
import ui from '@nuxt/ui/vue-plugin'
import { createSharedI18n, installApiInterceptors, useAuthStore } from 'floppa-web-shared'
import { client } from 'floppa-web-shared/client/client.gen'

import App from './App.vue'
import router from './router'

const app = createApp(App)

// Setup Pinia first (needed for auth store and Pinia Colada)
const pinia = createPinia()
app.use(pinia)
app.use(PiniaColada)

// Set active pinia so stores can be used outside component setup (Pinia 3 requirement)
setActivePinia(pinia)

// Setup i18n and Nuxt UI
app.use(createSharedI18n())
app.use(ui)

// Configure API client with auth interceptors. No X-Client-Version: the admin panel is served
// by the same binary that would check it, so it can never be out of date.
installApiInterceptors(client, useAuthStore())

app.use(router)

app.mount('#app')
