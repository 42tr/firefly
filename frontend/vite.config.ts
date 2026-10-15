import { defineConfig, loadEnv, type HttpProxy } from 'vite'
import vue from '@vitejs/plugin-vue'
import type { IncomingMessage } from 'node:http'
import { fileURLToPath } from 'node:url'

const proxyTarget = 'http://localhost:3003'
const configureProxy = (proxy: HttpProxy.Server) => {
  // Release backends mark the session cookie Secure; drop that flag so the
  // cookie survives on plain-HTTP dev origins (e.g. LAN access).
  proxy.on('proxyRes', (res) => {
    const cookies = res.headers['set-cookie']
    if (cookies) res.headers['set-cookie'] = cookies.map(cookie => cookie.replace(/;\s*secure/gi, ''))
  })
}

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
      '/api': { target: proxyTarget, changeOrigin: true, ws: true, configure: configureProxy },
      '/downloads': { target: proxyTarget, changeOrigin: true, configure: configureProxy },
      // Page navigations to /auth/ must render the dev SPA; the backend serves
      // its bundled (stale) build there. Only API calls (login/logout POSTs)
      // are proxied through.
      '/auth': {
        target: proxyTarget,
        changeOrigin: true,
        configure: configureProxy,
        bypass: (req: IncomingMessage) => ['GET', 'HEAD'].includes(req.method || '') ? '/index.html' : undefined,
      },
    },
  },
})
