import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.e2e.ts',
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: [['list'], ['html', { open: 'never' }]],
  use: {
    baseURL: 'http://127.0.0.1:5177',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'just dev',
    cwd: '..',
    url: 'http://127.0.0.1:5177/api/version',
    env: {
      FLOPPA_DEV_INSTANCE: 'e2e',
      FLOPPA_DEV_WEB_PORT: '5177',
      FLOPPA_DEV_API_PORT: '3117',
      FLOPPA_DEV_DB_PORT: '54339',
    },
    reuseExistingServer: false,
    timeout: 600_000,
    gracefulShutdown: { signal: 'SIGTERM', timeout: 15_000 },
  },
})
