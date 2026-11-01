import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import { fileURLToPath } from 'node:url'

export default defineConfig({
  root: fileURLToPath(new URL('.', import.meta.url)),
  plugins: [react()],
  test: {
    environment: 'jsdom',
    setupFiles: [fileURLToPath(new URL('./src/test/setup.ts', import.meta.url))],
    include: ['src/**/*.test.{ts,tsx}'],
  },
})
