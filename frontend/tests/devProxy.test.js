import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { createServer as createHttpServer, request } from 'node:http'
import { fileURLToPath } from 'node:url'
import test from 'node:test'
import { createServer, loadConfigFromFile } from 'vite'

function websocketHandshake(url, origin) {
  return new Promise((resolve, reject) => {
    const headers = {
      Connection: 'Upgrade',
      Upgrade: 'websocket',
      'Sec-WebSocket-Version': '13',
      'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==',
    }
    if (origin !== undefined) headers.Origin = origin
    const req = request(url, { headers })
    req.on('error', reject)
    req.setTimeout(5000, () => req.destroy(new Error('WebSocket handshake timed out')))
    req.on('upgrade', (response, socket) => {
      socket.destroy()
      resolve(response)
    })
    req.on('response', response => {
      response.resume()
      reject(new Error(`Unexpected WebSocket status: ${response.statusCode}`))
    })
    req.end()
  })
}

test('development proxy translates its own origin and preserves the local login flow', { timeout: 30000 }, async t => {
  const backend = createHttpServer((req, res) => {
    req.resume()
    res.writeHead(200, {
      'Content-Type': 'application/json',
      'Set-Cookie': [
        'firefly_session=test-session; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=604800',
        'firefly_login=; Path=/auth; Max-Age=0',
      ],
    })
    res.end(JSON.stringify({ origin: req.headers.origin ?? null }))
  })
  backend.on('upgrade', (req, socket) => {
    const accept = createHash('sha1')
      .update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11')
      .digest('base64')
    socket.end([
      'HTTP/1.1 101 Switching Protocols',
      'Upgrade: websocket',
      'Connection: Upgrade',
      `Sec-WebSocket-Accept: ${accept}`,
      `X-Upstream-Origin: ${req.headers.origin ?? '<absent>'}`,
      '', '',
    ].join('\r\n'))
  })
  backend.listen(0, '127.0.0.1')
  await once(backend, 'listening')
  t.after(() => new Promise((resolve, reject) => {
    backend.close(error => error ? reject(error) : resolve())
    backend.closeAllConnections()
  }))

  const { config } = await loadConfigFromFile(
    { command: 'serve', mode: 'development' },
    fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
  )
  const target = `http://127.0.0.1:${backend.address().port}`
  for (const rule of Object.values(config.server.proxy)) rule.target = target
  const vite = await createServer({
    ...config,
    configFile: false,
    logLevel: 'error',
    optimizeDeps: { noDiscovery: true, include: [] },
    server: {
      ...config.server,
      host: '127.0.0.1',
      port: 0,
      hmr: false,
      watch: null,
      preTransformRequests: false,
    },
  })
  t.after(() => vite.close())
  await vite.listen()
  const base = `http://127.0.0.1:${vite.httpServer.address().port}`

  await t.test('HTTP proxy forwards foreign Origins, including null and absence', async () => {
    for (const path of ['/api/check', '/downloads/check', '/auth/']) {
      for (const origin of [undefined, 'null', 'http://localhost:5173', 'http://192.168.1.3:5173']) {
        const headers = { 'Content-Type': 'application/json' }
        if (origin !== undefined) headers.Origin = origin
        const response = await fetch(base + path, { method: 'POST', headers, body: '{}' })
        assert.equal(response.status, 200)
        assert.deepEqual(await response.json(), { origin: origin ?? null })
      }
    }
  })

  await t.test('HTTP proxy translates its same origin to the backend origin', async () => {
    for (const path of ['/api/check', '/downloads/check', '/auth/']) {
      const response = await fetch(base + path, {
        method: 'POST', headers: { 'Content-Type': 'application/json', Origin: base }, body: '{}',
      })
      assert.equal(response.status, 200)
      assert.deepEqual(await response.json(), { origin: target })
    }
    const differentScheme = base.replace('http:', 'https:')
    const response = await fetch(base + '/auth/', { method: 'POST', headers: { Origin: differentScheme } })
    assert.deepEqual(await response.json(), { origin: differentScheme })
  })

  await t.test('WebSocket proxy forwards foreign Origins, including null and absence', async () => {
    for (const origin of [undefined, 'null', 'http://localhost:5173', 'http://192.168.1.3:5173']) {
      const response = await websocketHandshake(base + '/api/assistant/ws', origin)
      assert.equal(response.statusCode, 101)
      assert.equal(response.headers['x-upstream-origin'], origin ?? '<absent>')
    }
  })

  await t.test('WebSocket proxy translates its same origin to the backend origin', async () => {
    const response = await websocketHandshake(base + '/api/assistant/ws', base)
    assert.equal(response.statusCode, 101)
    assert.equal(response.headers['x-upstream-origin'], target)
  })

  await t.test('login navigation renders the current development SPA', async () => {
    for (const method of ['GET', 'HEAD']) {
      const response = await fetch(base + '/auth/?url=%2F', { method })
      assert.equal(response.status, 200)
      assert.match(response.headers.get('content-type'), /text\/html/)
      const body = await response.text()
      if (method === 'GET') assert.match(body, /\/@vite\/client/)
      else assert.equal(body, '')
    }
  })

  await t.test('HTTP development login keeps the session cookie attributes while removing Secure', async () => {
    const response = await fetch(base + '/auth/', { method: 'POST' })
    await response.text()
    const cookies = response.headers.getSetCookie()
    assert.equal(cookies.length, 2)
    assert.equal(cookies[0], 'firefly_session=test-session; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800')
    assert.equal(cookies[1], 'firefly_login=; Path=/auth; Max-Age=0')
  })
})
