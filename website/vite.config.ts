import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath } from 'node:url'

export default defineConfig({
  root: fileURLToPath(new URL('.', import.meta.url)),
  base: '/brr/',
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {'@': fileURLToPath(new URL('../src', import.meta.url))},
    dedupe: ['react', 'react-dom'],
  },
  build: {
    rollupOptions: {
      input: {
        website: fileURLToPath(new URL('./index.html', import.meta.url)),
        preview: fileURLToPath(new URL('./app-preview.html', import.meta.url)),
      },
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: [fileURLToPath(new URL('./src/test/setup.ts', import.meta.url))],
    include: ['src/**/*.test.{ts,tsx}'],
  },
})
