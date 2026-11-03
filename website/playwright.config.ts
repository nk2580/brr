import {defineConfig} from '@playwright/test'
import {fileURLToPath} from 'node:url'

export default defineConfig({
  testDir: './e2e',
  use: {baseURL: 'http://127.0.0.1:4174/brr/', viewport: {width: 1280, height: 900}, screenshot: 'only-on-failure'},
  projects: [{name: 'chromium', use: {browserName: 'chromium'}}],
  webServer: {
    cwd: fileURLToPath(new URL('.', import.meta.url)),
    command: 'pnpm build && pnpm exec vite preview --host 127.0.0.1 --port 4174 --strictPort',
    url: 'http://127.0.0.1:4174/brr/',
    timeout: 120000,
    reuseExistingServer: false,
  },
})
