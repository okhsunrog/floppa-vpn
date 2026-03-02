import './assets/main.css'

import { createApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { PiniaColada } from '@pinia/colada'
import ui from '@nuxt/ui/vue-plugin'
import { useAuthStore } from 'floppa-web-shared'
import { client } from 'floppa-web-shared/client/client.gen'

import { i18n } from './i18n'
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
app.use(i18n)
app.use(ui)

// Configure API client with auth interceptors
const authStore = useAuthStore()

client.interceptors.request.use((request) => {
  const token = authStore.getToken()
  if (token) {
    request.headers.set('Authorization', `Bearer ${token}`)
  }
  return request
})

client.interceptors.response.use((response) => {
  if (response.status === 401) {
    authStore.logout()
  }
  return response
})

app.use(router)

app.mount('#app')
