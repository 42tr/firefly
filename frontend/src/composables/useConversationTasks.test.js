import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { effectScope, nextTick, ref } from 'vue'
import { createServer } from 'vite'

const server = await createServer({ configFile: 'frontend/vite.config.ts', server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { createConversationTasks, useConversationTasks } = await server.ssrLoadModule('/src/composables/useConversationTasks.js')
const flush = async () => { await nextTick(); await new Promise(resolve => setImmediate(resolve)) }
const response = (tasks = [], cursor = 1, has_more = false, tool_cursor = 0) => ({ ok: true, json: async () => ({ tasks, cursor, has_more, tool_cursor }) })
const task = (job, status = 'running') => ({ id: job, generation_job_id: job, status, detached: true })

function setup(t) {
  // Keep this clock local: Vite's SSR loader also uses global timers. Mocking
  // those timers can cancel unrelated loader work when each test resets them.
  let now = 0, nextId = 0
  const pending = new Map()
  const timers = {
    setTimeout(callback, delay) { const id = ++nextId; pending.set(id, { callback, at: now + delay }); return id },
    clearTimeout(id) { pending.delete(id) },
    tick(ms) {
      now += ms
      while (true) {
        const due = [...pending].filter(([, timer]) => timer.at <= now).sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0]
        if (!due) return
        pending.delete(due[0])
        due[1].callback()
      }
    },
  }
  const calls = []
  const source = createConversationTasks((url, options) => new Promise(resolve => calls.push({ url, signal: options.signal, resolve })), timers)
  function mount(id = 1, job = 'job', status = 'completed', enabled = true) {
    const scope = effectScope(), conversation = ref(id), jobId = ref(job), replyStatus = ref(status), active = ref(enabled)
    const state = scope.run(() => useConversationTasks(() => conversation.value, () => jobId.value, () => replyStatus.value, () => active.value, source))
    t.after(() => scope.stop())
    return { ...state, scope, conversation, jobId, replyStatus, active }
  }
  return { calls, mount, timers }
}

test('eight reply trees share one request, snapshot, cursor and polling loop', async t => {
  const { calls, mount, timers } = setup(t)
  const trees = Array.from({ length: 8 }, (_, i) => mount(1, `job-${i}`))
  timers.tick(0)
  assert.equal(calls.length, 1)
  assert.match(calls[0].url, /\/conversations\/1\/tasks\?after=0&tools_after=0&wait_secs=0$/)
  const tasks = trees.map((_, i) => task(`job-${i}`))
  calls[0].resolve(response(tasks, 1718)); await flush()
  for (const tree of trees) assert.deepEqual(tree.tasks.value, tasks)
  timers.tick(250)
  assert.equal(calls.length, 2)
  assert.match(calls[1].url, /after=1718&tools_after=0&wait_secs=20$/)
  timers.tick(20000); await flush()
  assert.equal(calls.length, 2, 'no overlapping poll while the server is waiting')
  calls[1].resolve(response(tasks.map(item => ({ ...item, status: 'succeeded' })), 3515)); await flush()
  timers.tick(30000); await flush()
  assert.equal(calls.length, 2, 'terminal replies and tasks stop polling')
})

test('running replies poll before tasks exist and background tasks continue after a reply completes', async t => {
  const { calls, mount, timers } = setup(t)
  const tree = mount(1, 'job', 'running')
  timers.tick(0)
  calls[0].resolve(response([], 10)); await flush()
  timers.tick(250)
  assert.equal(calls.length, 2)
  calls[1].resolve(response([task('job')], 11)); await flush()
  tree.replyStatus.value = 'completed'; await flush(); timers.tick(0)
  assert.equal(calls.length, 3, 'completion requests a fresh shared snapshot')
  calls[2].resolve(response([task('job')], 12)); await flush()
  assert.equal(tree.tasks.value[0].status, 'running')
  timers.tick(250)
  assert.equal(calls.length, 4)
  assert.match(calls[3].url, /after=12&tools_after=0&wait_secs=20$/)
})

test('child agent progress advances its own cursor without durable task events', async t => {
  const { calls, mount, timers } = setup(t)
  const tree = mount(1, 'job', 'running')
  timers.tick(0)
  calls[0].resolve(response([{ ...task('job'), kind: 'handoff', detached: false }], 0, false, 5)); await flush()
  timers.tick(250)
  assert.match(calls[1].url, /after=0&tools_after=5&wait_secs=20$/)
  calls[1].resolve(response([{ ...task('job', 'succeeded'), kind: 'handoff' }], 0, false, 6)); await flush()
  tree.replyStatus.value = 'completed'; await flush(); timers.tick(0)
  assert.match(calls[2].url, /tools_after=6&wait_secs=0$/)
  calls[2].resolve(response([{ ...task('job', 'succeeded'), kind: 'handoff' }], 0, false, 6)); await flush()
  timers.tick(30000)
  assert.equal(calls.length, 3)
})

test('status updates and cancel refreshes coalesce and discard aborted responses', async t => {
  const { calls, mount, timers } = setup(t)
  const first = mount(1, 'first', 'running'), second = mount(1, 'second', 'running')
  timers.tick(0)
  first.replyStatus.value = 'completed'; second.replyStatus.value = 'completed'
  await flush()
  first.refresh(); second.refresh()
  timers.tick(0)
  assert.equal(calls.length, 2)
  assert.equal(calls[0].signal.aborted, true)
  calls[1].resolve(response([task('second', 'cancelled')], 20)); await flush()
  calls[0].resolve(response([task('first')], 10)); await flush()
  assert.deepEqual(first.tasks.value, [task('second', 'cancelled')])
  assert.deepEqual(second.tasks.value, first.tasks.value)
  timers.tick(30000)
  assert.equal(calls.length, 2)
})

test('only the last hidden or unmounted tree aborts a shared read and reopening gets fresh state', async t => {
  const { calls, mount, timers } = setup(t)
  const first = mount(), second = mount()
  timers.tick(0)
  first.active.value = false; await flush()
  assert.equal(calls[0].signal.aborted, false)
  second.scope.stop()
  assert.equal(calls[0].signal.aborted, true)
  timers.tick(30000)
  assert.equal(calls.length, 1)
  first.active.value = true; await flush(); timers.tick(0)
  assert.equal(calls.length, 2)
  assert.match(calls[1].url, /after=0&tools_after=0&wait_secs=0$/)
  calls[1].resolve(response([task('job', 'succeeded')], 30)); await flush()
  calls[0].resolve(response([task('job')], 10)); await flush()
  assert.equal(first.tasks.value[0].status, 'succeeded')
})

test('navigation separates conversations and ignores late data from the previous one', async t => {
  const { calls, mount, timers } = setup(t)
  const tree = mount(), hidden = mount(2, 'other', 'running', false)
  timers.tick(0)
  assert.equal(calls.length, 1)
  tree.conversation.value = 2; tree.jobId.value = 'other'; await flush(); timers.tick(0)
  assert.equal(calls[0].signal.aborted, true)
  assert.match(calls[1].url, /\/conversations\/2\/tasks\?after=0&tools_after=0&wait_secs=0$/)
  calls[1].resolve(response([task('other', 'succeeded')], 20)); await flush()
  calls[0].resolve(response([task('job')], 10)); await flush()
  assert.deepEqual(tree.tasks.value, [task('other', 'succeeded')])
  assert.deepEqual(hidden.tasks.value, [])
})

test('shared errors retry once and task events drain pages without duplicate requests', async t => {
  const { calls, mount, timers } = setup(t)
  const first = mount(), second = mount()
  timers.tick(0)
  calls[0].resolve({ ok: false }); await flush()
  assert.ok(first.error.value)
  assert.equal(second.error.value, first.error.value)
  timers.tick(2999); assert.equal(calls.length, 1)
  timers.tick(1); assert.equal(calls.length, 2)
  calls[1].resolve(response([task('job', 'succeeded')], 256, true)); await flush()
  assert.equal(first.error.value, '')
  timers.tick(0)
  assert.equal(calls.length, 3)
  assert.match(calls[2].url, /after=256&tools_after=0&wait_secs=20$/)
  calls[2].resolve(response([task('job', 'succeeded')], 270)); await flush()
  timers.tick(30000)
  assert.equal(calls.length, 3)
})
