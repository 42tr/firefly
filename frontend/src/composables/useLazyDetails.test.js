import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createServer } from 'vite'
import { effectScope, ref, nextTick } from 'vue'
const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { useLazyDetails } = await server.ssrLoadModule('/src/composables/useLazyDetails.js')
const flush = async () => { await nextTick(); await new Promise(resolve => setImmediate(resolve)) }
const response = data => ({ ok: true, json: async () => data })

test('no I/O while closed, one request on open, cache terminal results, reload when status changes', async () => {
  const scope = effectScope(), active = ref(false), status = ref('completed')
  let calls = 0
  const state = scope.run(() => useLazyDetails(() => '/tool-detail', () => status.value, active, async () => {
    calls++; return response({ input: { command: 'private' }, output: 'result' })
  }))
  await flush(); assert.equal(calls, 0)
  active.value = true; await flush(); assert.equal(calls, 1)
  assert.equal(state.detail.value.output, 'result')
  active.value = false; await flush(); active.value = true; await flush(); assert.equal(calls, 1)
  status.value = 'error'; await flush(); assert.equal(calls, 2)
  scope.stop()
})

test('close and navigation abort reads; late data never overwrites the current row', async () => {
  const scope = effectScope(), active = ref(false), url = ref('/first'), calls = []
  const state = scope.run(() => useLazyDetails(() => url.value, () => 'running', active,
    (endpoint, options) => new Promise(resolve => calls.push({ endpoint, signal: options.signal, resolve }))))
  active.value = true; await flush()
  active.value = false; await flush(); assert.equal(calls[0].signal.aborted, true)
  calls[0].resolve(response({ output: 'stale' })); await flush(); assert.equal(state.detail.value, null)
  active.value = true; await flush()
  url.value = '/second'; await flush(); assert.equal(calls[1].signal.aborted, true)
  calls[2].resolve(response({ output: 'current' })); await flush()
  calls[1].resolve(response({ output: 'old' })); await flush(); assert.equal(state.detail.value.output, 'current')
  scope.stop(); assert.equal(calls[2].signal.aborted, true)
})

test('failed reads show retry without restarting hidden reads', async () => {
  const scope = effectScope(), active = ref(true)
  let calls = 0
  const state = scope.run(() => useLazyDetails(() => '/tool-detail', () => 'completed', active,
    async () => { calls++; return calls === 1 ? { ok: false } : response({ output: 'ok' }) }))
  await flush(); assert.ok(state.error.value); assert.equal(calls, 1)
  state.retry(); await flush(); assert.equal(state.error.value, ''); assert.equal(state.detail.value.output, 'ok')
  scope.stop()
})
