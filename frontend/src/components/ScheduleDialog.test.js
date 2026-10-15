import assert from 'node:assert/strict'
import test from 'node:test'
import { createRenderer, ssrContextKey } from 'vue'
import { createServer } from 'vite'

const server = await createServer({ configFile: 'frontend/vite.config.ts', server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
test.after(() => server.close())
const { default: Dialog } = await server.ssrLoadModule('/src/components/ScheduleDialog.vue')
function setup(t) {
  let state
  const events = []
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = createApp({ ...Dialog, setup(props, context) { state = Dialog.setup(props, context); return state }, render: () => null },
    { open: false, onClose: () => events.push('close'), onOpenConversation: id => events.push(id) })
  app.provide(ssrContextKey, { modules: new Set() })
  app.mount({})
  t.after(() => app.unmount())
  return { state, events }
}
const task = { id: 12, title: '远程巡检', prompt: '检查并总结', cron: '0 9 * * 1-5', enabled: false,
  execution: { agent: 'claude', workspace_mode: 'shared', cwd: 'dev:project', provider: null, model: null, reasoning_level: null } }

test('schedule times use the server timezone instead of the browser timezone', t => {
  const { state } = setup(t)
  state.timezone.value = 'America/New_York'
  const instant = '2026-03-08T13:00:00Z'
  assert.equal(state.formatTime(instant), new Date(instant).toLocaleString('zh-CN', { timeZone: 'America/New_York', hour12: false }))
})

test('editing a schedule preserves the selected Agent, remote workspace, cron and disabled state', async t => {
  const { state } = setup(t)
  let payload, method, path
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    path = url; method = options.method; payload = JSON.parse(options.body)
    return { ok: true, json: async () => ({ id: task.id, ...payload }) }
  })
  await state.edit(task)
  state.prompt.value = '更新后的巡检要求'
  await state.save()
  assert.equal(method, 'PUT')
  assert.equal(path, '/api/assistant/schedules/12')
  assert.deepEqual(payload.execution, task.execution)
  assert.equal(payload.enabled, false)
  assert.equal(payload.cron, task.cron)
  assert.equal(payload.prompt, '更新后的巡检要求')
  assert.equal(state.editingId.value, null)
})

test('a failed save retains the task draft and displays the server error', async t => {
  const { state } = setup(t)
  t.mock.method(globalThis, 'fetch', async () => ({ ok: false, json: async () => ({ error: '目录不可用' }) }))
  await state.edit(task)
  await state.save()
  assert.equal(state.editingId.value, task.id)
  assert.equal(state.prompt.value, task.prompt)
  assert.equal(state.error.value, '目录不可用')
})

test('opening a run closes the dialog and selects its execution conversation', t => {
  const { state, events } = setup(t)
  state.viewRun({ last_conversation_id: 99 })
  assert.deepEqual(events, ['close', 99])
})
