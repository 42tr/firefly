import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createServer } from 'vite'

const server = await createServer({ configFile: 'frontend/vite.config.ts', server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
globalThis.window = undefined
after(() => { delete globalThis.window })
const { useAuth, loginReturnPath } = await server.ssrLoadModule('/src/composables/useAuth.ts')

test('expired sessions show the local form and connection errors allow retry', async t => {
  const auth = useAuth()
  t.mock.method(globalThis, 'fetch', async () => ({ ok: false, status: 401 }))
  await auth.check()
  assert.equal(auth.isReady.value, false)
  assert.equal(auth.isChecking.value, false)
  assert.equal(auth.error.value, '')
  t.mock.method(globalThis, 'fetch', async () => { throw new Error('offline') })
  await auth.check()
  assert.match(auth.error.value, /重新|重试/)
  t.mock.method(globalThis, 'fetch', async () => ({ ok: true }))
  await auth.check()
  assert.equal(auth.isReady.value, true)
  assert.equal(auth.error.value, '')
})

test('password login sends no identity and reports incorrect or throttled passwords', async t => {
  const auth = useAuth()
  let status = 401
  t.mock.method(globalThis, 'fetch', async (url, init) => {
    assert.equal(url, '/auth/')
    assert.equal(init.method, 'POST')
    assert.equal(init.credentials, 'include')
    assert.deepEqual(JSON.parse(init.body), { password: 'private-password' })
    return { ok: status === 204, status, headers: new Headers({ 'Retry-After': '3600' }) }
  })
  assert.equal(await auth.login('private-password'), false)
  assert.equal(auth.isReady.value, false)
  assert.equal(auth.error.value, '访问密码错误')
  status = 429
  assert.equal(await auth.login('private-password'), false)
  assert.equal(auth.error.value, '登录失败次数过多，该 IP 已封禁，请在 60 分钟后重试')
  status = 204
  assert.equal(await auth.login('private-password'), true)
  assert.equal(auth.isReady.value, true)
  assert.equal(auth.isSubmitting.value, false)
  assert.equal(auth.error.value, '')
})

test('logout clears readiness only when the server confirms or the session has expired', async t => {
  const replacements = []
  t.mock.property(globalThis, 'window', { location: { replace: path => replacements.push(path) } })
  const auth = useAuth()
  auth.isReady.value = true
  t.mock.method(globalThis, 'fetch', async () => ({ ok: false, status: 500 }))
  assert.equal(await auth.logout(), false)
  assert.equal(auth.isReady.value, true)
  assert.deepEqual(replacements, [])
  t.mock.method(globalThis, 'fetch', async (url, init) => {
    assert.equal(url, '/auth/logout')
    assert.equal(init.method, 'POST')
    return { ok: false, status: 401 }
  })
  assert.equal(await auth.logout(), true)
  assert.equal(auth.isReady.value, false)
  assert.deepEqual(replacements, ['/'])
})

test('banned login shows remaining minutes and falls back when Retry-After is missing', async t => {
  for (const [retryAfter, minutes] of [['61', 2], ['1', 1], [null, 60]]) {
    const auth = useAuth()
    const headers = new Headers()
    if (retryAfter !== null) headers.set('Retry-After', retryAfter)
    t.mock.method(globalThis, 'fetch', async () => ({ ok: false, status: 429, headers }))
    assert.equal(await auth.login('private-password'), false)
    assert.equal(auth.error.value, `登录失败次数过多，该 IP 已封禁，请在 ${minutes} 分钟后重试`)
    assert.equal(auth.isReady.value, false)
    assert.equal(auth.isSubmitting.value, false)
  }
})

test('return paths preserve in-app state and reject external redirects and auth loops', t => {
  t.mock.property(globalThis, 'window', { location: { origin: 'https://firefly.example' } })
  assert.equal(loginReturnPath('/?view=scripts#section'), '/?view=scripts#section')
  assert.equal(loginReturnPath('https://firefly.example/?conversation=42'), '/?conversation=42')
  for (const value of [null, '//evil.example', 'https://evil.example/', '/\\evil.example', '/auth/', 'https://user@firefly.example/', '/\n/evil.example']) {
    assert.equal(loginReturnPath(value), '/')
  }
})
