import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createServer } from 'vite'
const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { createAssistantRealtime } = await server.ssrLoadModule('/src/composables/useAssistantRealtime.js')

function setup(t, fetcher = async () => ({ ok: true })) {
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] })
  const sockets = []
  class Socket {
    readyState = 0
    sent = []
    constructor(url) { this.url = String(url); sockets.push(this) }
    open() { this.readyState = 1; this.onopen?.() }
    send(data) { this.sent.push(JSON.parse(data)) }
    receive(data) { this.onmessage?.({ data: JSON.stringify(data) }) }
    close() { this.readyState = 3; this.onclose?.() }
  }
  const app = createAssistantRealtime({ WebSocketClass: Socket, href: () => 'https://example.com/assistant/', fetcher })
  t.after(() => app.stop())
  return { app, sockets }
}

test('one socket shares subscriptions, pushes data and stops only after the last subscriber leaves', t => {
  const { app, sockets } = setup(t)
  const first = [], second = []
  const a = app.subscribe('pending', {}, data => first.push(data))
  const b = app.subscribe('pending', {}, data => second.push(data))
  app.start(); app.start()
  assert.equal(sockets.length, 1)
  const ws = sockets[0]; ws.open()
  assert.equal(ws.url, 'wss://example.com/api/assistant/ws')
  assert.equal(ws.sent.length, 1)
  const id = ws.sent[0].id
  ws.receive({ type: 'snapshot', id, data: { todo_count: 2 } })
  assert.deepEqual(first, [{ todo_count: 2 }]); assert.deepEqual(first, second)
  a.stop(); assert.equal(ws.sent.length, 1)
  b.refresh(); assert.deepEqual(ws.sent.at(-1), { type: 'refresh', id })
  b.stop(); assert.deepEqual(ws.sent.at(-1), { type: 'unsubscribe', id })
  ws.receive({ type: 'snapshot', id, data: { todo_count: 99 } })
  assert.equal(second.length, 1)
})

test('reconnect resubscribes and recovers changes missed offline; stale sockets cannot overwrite snapshots', async t => {
  let httpReads = 0
  const { app, sockets } = setup(t, async () => { httpReads++; return { ok: true } })
  const received = []
  app.subscribe('conversations', {}, data => received.push(data))
  app.start(); sockets[0].open()
  const id = sockets[0].sent[0].id
  sockets[0].receive({ type: 'snapshot', id, data: 'old' })
  sockets[0].close(); assert.equal(app.connected.value, false)
  t.mock.timers.tick(1000)
  await Promise.resolve(); await Promise.resolve()
  assert.equal(sockets.length, 2)
  sockets[1].open()
  assert.equal(sockets[1].sent[0].id, id)
  sockets[1].receive({ type: 'snapshot', id, data: 'latest' })
  sockets[0].receive({ type: 'snapshot', id, data: 'stale' })
  assert.deepEqual(received, ['old', 'latest'])
  assert.equal(httpReads, 1) // Authentication recovery only, never a status poll.
})

test('heartbeat and local watchdog never fetch state, and stopped sessions discard cached private data', t => {
  let calls = 0
  const { app, sockets } = setup(t, async () => { calls++; return { ok: true } })
  let count = 0
  app.subscribe('pending', {}, () => { count++ })
  app.start(); sockets[0].open()
  const id = sockets[0].sent[0].id
  sockets[0].receive({ type: 'snapshot', id, data: { todo_count: 1 } })
  for (let i = 0; i < 10; i++) {
    sockets[0].receive({ type: 'heartbeat', server_time: Date.now() })
    t.mock.timers.tick(25000)
  }
  assert.equal(calls, 0)
  assert.equal(sockets[0].sent.length, 1)
  app.stop()
  app.subscribe('pending', {}, () => { count++ })
  assert.equal(count, 1)
  sockets[0].receive({ type: 'snapshot', id, data: { todo_count: 99 } })
  assert.equal(count, 1)
})

test('changing a panel subscription ignores late snapshots and reports subscription errors', t => {
  const { app, sockets } = setup(t)
  const values = [], errors = []
  app.start(); sockets[0].open()
  const old = app.subscribe('todos', { status: 'pending' }, data => values.push(data))
  const oldId = sockets[0].sent.at(-1).id
  old.stop()
  app.subscribe('todos', { status: 'completed' }, data => values.push(data), error => errors.push(error))
  const newId = sockets[0].sent.at(-1).id
  sockets[0].receive({ type: 'snapshot', id: oldId, data: 'wrong page' })
  sockets[0].receive({ type: 'subscription_error', id: newId, message: 'retry' })
  sockets[0].receive({ type: 'snapshot', id: newId, data: 'completed page' })
  assert.deepEqual(values, ['completed page']); assert.deepEqual(errors, ['retry'])
})

test('process deltas merge changed and removed records for shared listeners and reconnect from a full snapshot', async t => {
  const { app, sockets } = setup(t)
  const received = []
  app.subscribe('process', { conversation_id: 1, job_id: 'job' }, data => received.push(data))
  app.start(); sockets[0].open()
  const id = sockets[0].sent[0].id
  sockets[0].receive({ type: 'snapshot', id, data: { conversation_id: 1, job_id: 'job',
    messages: [{ id: 1, content: 'question' }, { id: 2, content: 'old tool' }, { id: 3, content: 'deleted' }] } })
  sockets[0].receive({ type: 'snapshot_delta', id, data: { conversation_id: 1, job_id: 'job',
    messages: [{ id: 2, content: 'updated tool' }, { id: 4, content: 'answer' }], removed_ids: [3] } })
  const expected = [{ id: 1, content: 'question' }, { id: 2, content: 'updated tool' }, { id: 4, content: 'answer' }]
  assert.deepEqual(received.at(-1).messages, expected)
  const shared = []
  app.subscribe('process', { conversation_id: 1, job_id: 'job' }, data => shared.push(data))
  assert.deepEqual(shared[0].messages, expected)
  sockets[0].close()
  t.mock.timers.tick(1000)
  await Promise.resolve(); await Promise.resolve()
  sockets[1].open()
  sockets[1].receive({ type: 'snapshot', id, data: { conversation_id: 1, job_id: 'job', messages: [{ id: 5, content: 'offline result' }] } })
  assert.deepEqual(shared.at(-1).messages, [{ id: 5, content: 'offline result' }])
  assert.deepEqual(received.at(-1), shared.at(-1))
})
