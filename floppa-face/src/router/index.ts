import { createRouter, createWebHistory } from 'vue-router'
import { createAppRoutes, installAuthGuard } from 'floppa-web-shared/router'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: createAppRoutes(),
})

installAuthGuard(router)

export default router
