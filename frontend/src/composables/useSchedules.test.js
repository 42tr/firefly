import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createServer } from 'vite'

const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
test.after(() => server.close())
const { useSchedules } = await server.ssrLoadModule('/src/composables/useSchedules.js')
const response = data => ({ ok: true, json: async () => data })

test('the server timezone drives schedule display', async t => {
  const app = useSchedules()
  t.mock.method(globalThis, 'fetch', async () => response({ schedules: [], timezone: 'America/New_York' }))
  await app.load()
  assert.equal(app.timezone.value, 'America/New_York')
})

test('saving retains Agent execution settings and invalidates stale list responses', async t => {
  const app = useSchedules()
  let finishLoad, submitted
  t.mock.method(globalThis, 'fetch', async (_url, options) => {
    if (options.method === 'POST') {
      submitted = JSON.parse(options.body)
      return response({ id: 1, ...submitted })
    }
    return new Promise(resolve => { finishLoad = resolve })
  })
  const loading = app.load()
  const execution = { agent: 'codex', cwd: 'project', workspace_mode: 'parallel', provider: 'relay', model: 'model-a', reasoning_level: 'high' }
  await app.mutate(null, 'POST', { title: '巡检', prompt: '检查项目', cron: '0 9 * * *', enabled: true, execution })
  finishLoad(response({ schedules: [] }))
  await loading
  assert.deepEqual(submitted.execution, execution)
  assert.equal(app.schedules.value[0].id, 1)
  assert.equal(app.loading.value, false)
})

test('failed saves preserve the list and expose the backend cron validation message', async t => {
  const app = useSchedules()
  app.schedules.value = [{ id: 1, title: '已有任务' }]
  t.mock.method(globalThis, 'fetch', async () => ({ ok: false, json: async () => ({ error: 'cron 表达式无效' }) }))
  assert.equal(await app.mutate(1, 'PUT', { cron: 'invalid' }), null)
  assert.equal(app.schedules.value[0].title, '已有任务')
  assert.equal(app.error.value, 'cron 表达式无效')
  assert.equal(app.saving.value, false)
})

test('duplicate mutations are blocked and disabling/deleting updates local state', async t => {
  const app = useSchedules()
  app.schedules.value = [{ id: 1, enabled: true }]
  let finish, calls = 0
  t.mock.method(globalThis, 'fetch', async (_url, options) => {
    calls += 1
    if (options.method === 'PATCH') return new Promise(resolve => { finish = resolve })
    return response({ ok: true })
  })
  const disabling = app.mutate(1, 'PATCH', { enabled: false })
  assert.equal(await app.mutate(1, 'DELETE'), null)
  finish(response({ id: 1, enabled: false, next_run_at: null }))
  await disabling
  assert.equal(app.schedules.value[0].enabled, false)
  await app.mutate(1, 'DELETE')
  assert.equal(app.schedules.value.length, 0)
  assert.equal(calls, 2)
})
