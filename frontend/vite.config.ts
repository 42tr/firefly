import { defineConfig, loadEnv } from 'vite'
import vue from '@vitejs/plugin-vue'
import { fileURLToPath } from 'node:url'

export default defineConfig({
  root: fileURLToPath(new URL('.', import.meta.url)),
  base: '/',
  plugins: [vue(), {
    name: 'firefly-app-timezone',
    transformIndexHtml(html, context) {
      if (!context.server) return html
      const root = fileURLToPath(new URL('../', import.meta.url))
      const timezone = loadEnv(context.server.config.mode, root, 'APP_TIMEZONE').APP_TIMEZONE || 'Asia/Shanghai'
      new Intl.DateTimeFormat('en', { timeZone: timezone })
      return html.replace(/name="firefly-timezone" content="[^"]*"/, `name="firefly-timezone" content="${timezone}"`)
    },
  }],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
    dedupe: ['vue'],
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes('/node_modules/zrender/')) return 'chart-renderer'
          if (id.includes('/node_modules/vue/') || id.includes('/node_modules/@vue/')) return 'vendor'
          if (id.includes('/node_modules/marked/') || id.includes('/node_modules/dompurify/')) return 'markdown'
        },
      },
    },
  },
  server: {
    proxy: {
      '/api': { target: 'http://localhost:3003', changeOrigin: true, ws: true },
      '/downloads': { target: 'http://localhost:3003', changeOrigin: true },
      '/auth': { target: 'http://localhost:3003', changeOrigin: true },
    },
  },
})
