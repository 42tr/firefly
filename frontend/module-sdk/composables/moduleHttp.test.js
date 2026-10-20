import test from 'node:test'
import assert from 'node:assert/strict'

test('six data sources share the four host slots and a failed request releases its slot', async t => {
  const old = globalThis.window
  let listener, active = 0, maximum = 0
  const parent = { postMessage(message) {
    active++; maximum = Math.max(maximum, active)
    setTimeout(() => {
      active--
      const result = message.params.arguments.path === '/failed'
        ? { isError: true, content: [{ type: 'text', text: 'provider unavailable' }] }
        : { structuredContent: { status: 200, headers: {}, body: { ok: true } } }
      listener({ source: parent, data: { jsonrpc: '2.0', id: message.id, result } })
    }, 5)
  } }
  globalThis.window = { parent, addEventListener: (_event, callback) => { listener = callback } }
  t.after(() => { if (old === undefined) delete globalThis.window; else globalThis.window = old })
  const { apiFetch } = await import('../utils/http.js')
  const results = await Promise.allSettled(['/one', '/failed', '/three', '/four', '/five', '/six'].map(path => apiFetch(path)))
  assert.equal(maximum, 4)
  assert.equal(active, 0)
  assert.equal(results.filter(result => result.status === 'fulfilled').length, 5)
  assert.match(results[1].reason.message, /provider unavailable/)
})
