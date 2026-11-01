import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  use: {baseURL: 'http://127.0.0.1:1420', viewport: {width:1280,height:860}, screenshot:'only-on-failure'},
  projects: [{name:'chromium', use:{browserName:'chromium'}}],
  webServer: {command:'pnpm dev', url:'http://127.0.0.1:1420', reuseExistingServer:!process.env.CI},
})
