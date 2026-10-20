import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createRenderer, ref } from 'vue'
import { createServer } from 'vite'
const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
test.after(() => server.close())
const { getJobStartedAt, useChatStream } = await server.ssrLoadModule('/src/composables/useChatStream.js')
const { newSubmissionId } = await server.ssrLoadModule('/src/composables/submissionIdentity.ts')
const { provideAssistantRealtime } = await server.ssrLoadModule('/src/composables/useAssistantRealtime.js')
const { useMessageTimeline } = await server.ssrLoadModule('/src/composables/useMessageTimeline.js')
const { getProcessDurationMs } = await server.ssrLoadModule('/src/composables/processDuration.js')

test('submission IDs remain available on HTTP intranet origins without randomUUID', () => {
  const ids = Array.from({ length: 30 }, () => newSubmissionId({
    getRandomValues: bytes => crypto.getRandomValues(bytes),
  }))
  for (const id of ids) assert.match(id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
  assert.equal(new Set(ids).size, ids.length)
})

function setup(options = {}) {
  const messages = ref([])
  const activeConversationId = ref(1)
  const chatAttachments = ref([])
  const allowed = ref(true)
  const isStreaming = ref(false)
  const conversationError = ref('')
  const stream = useChatStream({
    messages, activeConversationId, chatAttachments,
    isStreaming, conversationError,
    messageAttachmentUrls: new Set(),
    scrollToBottom: async () => {}, patchConversation: () => {}, fetchConversations: () => {},
    clearAttachments: () => { chatAttachments.value = [] },
    canSend: () => allowed.value,
    ...options,
  })
  return { ...stream, messages, activeConversationId, chatAttachments, allowed, isStreaming, conversationError }
}

function setupRealtime(t, options = {}) {
  let app
  const subscriptions = []
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const mounted = createApp({ setup() {
    const realtime = provideAssistantRealtime(ref(false))
    t.mock.method(realtime, 'subscribe', (topic, params, receive, error) => {
      const subscription = { topic, params, receive, error, stopped: false,
        refresh() {}, stop() { this.stopped = true } }
      subscriptions.push(subscription)
      return subscription
    })
    app = setup({ followQueue: false, ...options })
    return () => null
  } })
  mounted.mount({})
  t.after(() => { app.abortStream(); mounted.unmount() })
  return { ...app, subscriptions }
}

const sse = events => ({ ok: true, body: new ReadableStream({ start(controller) {
  for (const [type, payload] of events) {
    controller.enqueue(new TextEncoder().encode(`event: ${type}\ndata: ${JSON.stringify(payload)}\n\n`))
  }
  controller.close()
} }) })

const snapshotMessage = (id, role, content, status = 'completed') => ({
  id, role, content, generation_job_id: 'job-1', processing_status: status,
})
const transformSnapshot = records => records.map(record => ({
  ...record, jobId: record.generation_job_id, generationJobId: record.generation_job_id,
}))

test('native generated images are visible reply cards even when tools follow them', async t => {
  const app = setup({ followQueue: false, normalizeAttachments: files => files || [] })
  t.after(app.abortStream)
  const result = { type: 'image', image_url: '/api/assistant/attachments/by-file/generated', failure: null }
  t.mock.method(globalThis, 'fetch', async () => sse([
    ['tool_call', { seq: 1, id: 'drawing', name: 'image_generation', arguments: {} }],
    ['tool_result', { seq: 2, id: 'drawing', name: 'image_generation', presentation: result, message_id: 3,
      attachments: [{ id: 1, content_type: 'image/png' }] }],
    ['tool_call', { seq: 3, id: 'cleanup', name: 'command', arguments: {} }],
    ['tool_result', { seq: 4, id: 'cleanup', name: 'command', result: { exit_code: 0 } }],
    ['assistant_done', { seq: 5, last_message_id: 5 }],
  ]))
  await app.resumeJob('job-1', 1)
  const image = app.messages.value.find(message => message.kind === 'image-result')
  assert.equal(Math.max(...app.messages.value.map(message => message.readMessageId || 0)), 5)
  assert.equal(image.role, 'assistant')
  assert.deepEqual(image.imageOutput, result)
  assert.equal(image.messageId, 3)
  assert.deepEqual(image.attachments, [])
  assert.equal(app.messages.value.some(message => message.id === 'drawing' && message.kind === 'tool-run'), false)
  assert.equal(app.messages.value.find(message => message.id === 'cleanup').status, 'completed')
})

test('stream completion does not invent successful results for unfinished tools', async t => {
  const app = setup({ followQueue: false, normalizeAttachments: files => files || [] })
  t.after(app.abortStream)
  t.mock.method(globalThis, 'fetch', async () => sse([
    ['tool_call', { seq: 1, id: 'finished', name: 'lookup', arguments: {} }],
    ['tool_result', { seq: 2, id: 'finished', name: 'lookup', result: 'found', is_error: false }],
    ['tool_call', { seq: 3, id: 'unknown', name: 'write_file', arguments: {} }],
    ['assistant_done', { seq: 4 }],
  ]))
  await app.resumeJob('job-1', 1)
  assert.equal(app.messages.value.find(message => message.id === 'finished').status, 'completed')
  assert.equal(app.messages.value.find(message => message.id === 'unknown').status, 'interrupted')
  assert.equal(app.isStreaming.value, false)
})

test('automatic queue subscription does not replay a stopped job while its worker is cancelling', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  let app, receive
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const mounted = createApp({ setup() {
    const realtime = provideAssistantRealtime(ref(false))
    t.mock.method(realtime, 'subscribe', (topic, _params, callback) => {
      assert.equal(topic, 'active_generation')
      receive = callback
      return { refresh() {}, stop() {} }
    })
    app = setup()
    return () => null
  } })
  mounted.mount({})
  t.after(() => { app.abortStream(); mounted.unmount() })
  const streams = []
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    if (options.method === 'DELETE') return { ok: true }
    streams.push(url)
    return sse([['assistant_delta', { seq: 1, delta: streams.length === 1 ? 'stopped reply' : 'next reply' }]])
  })
  await app.resumeJob('job-1', 1)
  await app.stopGeneration()
  receive({ active: true, status: 'cancelling', job_id: 'job-1' })
  await new Promise(setImmediate)
  assert.equal(app.isStreaming.value, false)
  assert.equal(streams.length, 1)
  assert.equal(app.messages.value[0].content, 'stopped reply')
  receive({ active: true, status: 'running', job_id: 'job-2' })
  await new Promise(setImmediate)
  assert.equal(streams.length, 2)
  assert.match(streams[1], /\/jobs\/job-2\/stream/)
})

test('HTTP queue following waits for cancelling workers without replaying or completing their conversation', async t => {
  const patches = []
  const app = setup({ patchConversation: (_id, patch) => patches.push(patch) })
  t.after(app.abortStream)
  const streams = []
  t.mock.method(globalThis, 'fetch', async url => {
    if (url.endsWith('/active-generation')) return { ok: true,
      json: async () => ({ active: true, status: 'cancelling', job_id: 'job-2' }) }
    streams.push(url)
    return sse([['assistant_done', { seq: 1 }]])
  })
  await app.resumeJob('job-1', 1)
  await new Promise(setImmediate)
  assert.equal(streams.length, 1)
  assert.equal(app.isStreaming.value, false)
  assert.equal(patches.some(patch => patch.status === 'completed'), false)
})

test('resuming a queued turn moves its question into the timeline before its reply finishes', async t => {
  const app = setup({ followQueue: false })
  t.after(app.abortStream)
  app.messages.value = [
    { role: 'user', content: 'question', generationJobId: 'job-1', processingStatus: 'queued' },
    { role: 'user', content: 'question', generationJobId: 'job-2', processingStatus: 'queued' },
  ]
  let controller
  t.mock.method(globalThis, 'fetch', async () => ({ ok: true, body: new ReadableStream({ start(value) {
    controller = value
    controller.enqueue(new TextEncoder().encode(
      'event: stream_started\ndata: {"job_id":"job-1"}\n\nevent: assistant_delta\ndata: {"seq":1,"delta":"replying"}\n\n',
    ))
  } }) }))
  const streaming = app.resumeJob('job-1', 1)
  t.after(async () => { controller.close(); await streaming })
  await new Promise(setImmediate)
  assert.equal(app.isStreaming.value, true)
  assert.equal(app.messages.value[0].processingStatus, 'running')
  assert.deepEqual(app.queuedMessages.value.map(message => message.generationJobId), ['job-2'])
  const timeline = useMessageTimeline({ messages: app.messages, activeConversationId: app.activeConversationId })
  assert.ok(timeline.groupedMessages.value.some(item => item.msg === app.messages.value[0]))
  controller.enqueue(new TextEncoder().encode('event: assistant_done\ndata: {"seq":2}\n\n'))
  await new Promise(setImmediate)
  assert.equal(app.messages.value[0].processingStatus, 'completed')
})

for (const [status, acknowledgedStatus] of ['running', 'completed', 'failed', 'cancelled']
  .flatMap(status => ['queued', 'running'].map(acknowledgedStatus => [status, acknowledgedStatus]))) {
  test(`a delayed ${acknowledgedStatus} acknowledgement preserves the ${status} state already received over the stream`, async t => {
    const app = setupRealtime(t, { followQueue: true })
    const subscription = app.subscriptions.find(item => item.topic === 'active_generation')
    let acknowledge, controller, streamRequests = 0
    t.mock.method(globalThis, 'fetch', async (url, options) => {
      if (options.method === 'POST') return new Promise(resolve => { acknowledge = resolve })
      assert.match(url, /\/jobs\/job-1\/stream/)
      streamRequests++
      return { ok: true, body: new ReadableStream({ start(value) { controller = value } }) }
    })
    app.input.value = 'question'
    const sending = app.sendMessage()
    await new Promise(setImmediate)
    const question = app.messages.value[0]
    subscription.receive({ active: true, status: 'running', job_id: 'job-1' })
    await new Promise(setImmediate)
    t.after(() => controller.close())
    const events = [
      ['stream_started', { job_id: 'job-1', started_at: '2026-10-12T01:00:00Z' }],
      ['assistant_delta', { seq: 1, delta: 'reply' }],
      ...(status === 'failed' ? [['error', { seq: 2, message: 'upstream failed' }]] : []),
      ...(status !== 'running' ? [['assistant_done', { seq: 3, cancelled: status === 'cancelled' }]] : []),
    ]
    for (const [event, payload] of events) controller.enqueue(new TextEncoder().encode(
      `event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`,
    ))
    await new Promise(setImmediate)
    app.messages.value.push({ role: 'user', content: 'question', generationJobId: 'job-2', processingStatus: 'queued' })
    acknowledge({ ok: true, json: async () => ({ message: { id: 10 }, status: acknowledgedStatus, job_id: 'job-1' }) })
    await sending
    assert.equal(question.processingStatus, status === 'cancelled' ? 'failed' : status)
    assert.equal(question.processingError, status === 'failed' ? 'upstream failed' : status === 'cancelled' ? '用户已停止生成' : '')
    const timeline = useMessageTimeline({ messages: app.messages, activeConversationId: app.activeConversationId })
    assert.equal(timeline.groupedMessages.value.find(item => item.msg === question).timeValue, '2026-10-12T01:00:00.000Z')
    assert.deepEqual(app.queuedMessages.value.map(message => message.generationJobId), ['job-2'])
    assert.equal(app.messages.value.filter(message => message.role === 'assistant').length, status === 'failed' ? 2 : 1)
    assert.equal(app.isStreaming.value, status === 'running')
    assert.equal(streamRequests, 1)
    assert.deepEqual(app.getLocalSubmissions(1), [])
  })
}

for (const status of ['running', 'completed', 'failed']) {
  test(`a delayed queue acknowledgement does not overwrite ${status} history`, async t => {
    const app = setup({ followQueue: false })
    t.after(app.abortStream)
    let acknowledge
    t.mock.method(globalThis, 'fetch', url => url.includes('/stream?')
      ? sse([['stream_started', { job_id: 'job-1' }]])
      : new Promise(resolve => { acknowledge = resolve }))
    app.input.value = 'question'
    const sending = app.sendMessage()
    await new Promise(setImmediate)
    const pending = app.messages.value[0]
    const error = status === 'failed' ? 'upstream failed' : ''
    app.messages.value = [
      { role: 'user', id: 10, generationJobId: 'job-1', processingStatus: status, processingError: error },
      pending,
    ]
    const history = app.messages.value[0]
    acknowledge({ ok: true, json: async () => ({ message: { id: 10, processing_started_at: '2026-10-12T01:00:00Z' }, status: 'queued', job_id: 'job-1' }) })
    await sending
    assert.deepEqual(app.messages.value, [history])
    assert.equal(history.processingStatus, status)
    assert.equal(history.processingError, error)
    const timeline = useMessageTimeline({ messages: app.messages, activeConversationId: app.activeConversationId })
    assert.equal(timeline.groupedMessages.value.find(item => item.msg === history).timeValue, '2026-10-12T01:00:00.000Z')
    assert.equal(app.queuedMessages.value.length, 0)
  })
}

test('reconnect sends the execution identity together with the cursor', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  const app = setup({ followQueue: false })
  t.after(app.abortStream)
  const requests = []
  t.mock.method(globalThis, 'fetch', async url => {
    requests.push(url)
    return requests.length === 1
      ? sse([['stream_started', { job_id: 'job-1', execution_id: 'run-a', seq: 0 }],
        ['assistant_delta', { execution_id: 'run-a', seq: 1, delta: 'before ' }]])
      : sse([['stream_started', { job_id: 'job-1', execution_id: 'run-a', seq: 0 }],
        ['assistant_delta', { execution_id: 'run-a', seq: 2, delta: 'after' }],
        ['assistant_done', { execution_id: 'run-a', seq: 3 }]])
  })
  await app.resumeJob('job-1', 1)
  t.mock.timers.tick(1000)
  await new Promise(setImmediate)
  assert.match(requests[1], /after_seq=1&execution_id=run-a$/)
  assert.equal(app.messages.value[0].content, 'before after')
  assert.equal(app.isStreaming.value, false)
})

for (const reason of ['execution_changed', 'execution_recovered', 'log_expired', 'job_not_in_memory']) {
  test(`${reason} restores the whole job once and follows socket updates until completion`, async t => {
    t.mock.timers.enable({ apis: ['setTimeout'] })
    const app = setupRealtime(t, { transformServerMessages: transformSnapshot })
    t.after(app.abortStream)
    app.messages.value = [{ role: 'assistant', content: 'older job', jobId: 'older' },
      { role: 'user', content: 'question', generationJobId: 'job-1' },
      { role: 'user', content: 'next job', generationJobId: 'next' }]
    const records = [snapshotMessage(1, 'user', 'question', 'running'),
      ...Array.from({ length: 130 }, (_, i) => snapshotMessage(i + 2, 'assistant', `saved ${i}`, 'running'))]
    let snapshots = 0
    const requests = []
    t.mock.method(globalThis, 'fetch', async url => {
      requests.push(url)
      if (url.includes('/stream?')) return sse([
        ['assistant_delta', { execution_id: 'run-a', seq: 8, delta: 'stale fragment' }],
        ['stream_reset', { execution_id: 'run-b', seq: 0, reason, snapshot_required: true }],
        ['assistant_delta', { execution_id: 'run-a', seq: 9, delta: 'must ignore' }],
      ])
      assert.equal(url, '/api/assistant/conversations/1/process?job_id=job-1')
      snapshots++
      return { ok: true, json: async () => ({ messages: snapshots === 1 ? records
        : records.map(record => ({ ...record, processing_status: 'completed' })) }) }
    })
    await app.resumeJob('job-1', 1)
    assert.equal(app.messages.value.length, 133)
    assert.equal(app.messages.value.at(-1).content, 'next job')
    assert.equal(app.messages.value.some(message => /stale fragment|must ignore/.test(message.content)), false)
    assert.equal(app.isStreaming.value, true)
    const subscription = app.subscriptions[0]
    assert.equal(subscription.topic, 'process')
    assert.deepEqual(subscription.params, { conversation_id: 1, job_id: 'job-1' })
    t.mock.timers.tick(30000)
    await new Promise(setImmediate)
    assert.equal(snapshots, 1)
    assert.equal(app.isStreaming.value, true)
    subscription.receive({ messages: records.map(record => ({ ...record, processing_status: 'completed' })) })
    assert.equal(app.isStreaming.value, false)
    assert.equal(subscription.stopped, true)
    t.mock.timers.tick(30000)
    assert.equal(snapshots, 1)
    assert.equal(requests.filter(url => url.includes('/stream?')).length, 1)
  })
}

test('a missing SSE job immediately recovers its committed answer without replay retries', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  const app = setup({ followQueue: false, transformServerMessages: transformSnapshot })
  t.after(app.abortStream)
  const requests = []
  t.mock.method(globalThis, 'fetch', async url => {
    requests.push(url)
    return url.includes('/stream?') ? { ok: false, status: 404 }
      : { ok: true, json: async () => ({ messages: [snapshotMessage(1, 'user', 'question'),
        snapshotMessage(2, 'assistant', 'committed answer')] }) }
  })
  await app.resumeJob('job-1', 1)
  assert.equal(app.messages.value.at(-1).content, 'committed answer')
  assert.equal(app.isStreaming.value, false)
  t.mock.timers.tick(300000)
  assert.equal(requests.length, 2)
})

test('a changed execution is checked before its lower sequence number', async t => {
  const app = setup({ followQueue: false, transformServerMessages: transformSnapshot })
  t.after(app.abortStream)
  t.mock.method(globalThis, 'fetch', async url => url.includes('/stream?') ? sse([
    ['assistant_delta', { execution_id: 'old-run', seq: 80, delta: 'old answer fragment' }],
    ['stream_started', { execution_id: 'new-run', seq: 0 }],
  ]) : { ok: true, json: async () => ({ messages: [snapshotMessage(1, 'user', 'question'),
    snapshotMessage(2, 'assistant', 'new complete answer')] }) })
  await app.resumeJob('job-1', 1)
  assert.equal(app.messages.value.at(-1).content, 'new complete answer')
  assert.equal(app.isStreaming.value, false)
})

test('switching conversations prevents a late recovery snapshot from replacing the new conversation', async t => {
  const app = setup({ followQueue: false, transformServerMessages: transformSnapshot })
  let resolveSnapshot
  t.mock.method(globalThis, 'fetch', async url => url.includes('/stream?') ? { ok: false, status: 404 }
    : { ok: true, json: () => new Promise(resolve => { resolveSnapshot = resolve }) })
  const recovering = app.resumeJob('job-1', 1)
  await new Promise(setImmediate)
  app.abortStream()
  app.activeConversationId.value = 2
  app.messages.value = [{ role: 'assistant', content: 'another conversation' }]
  resolveSnapshot({ messages: [snapshotMessage(2, 'assistant', 'stale recovery')] })
  await recovering
  assert.equal(app.messages.value[0].content, 'another conversation')
})

test('root tool and handoff failures keep their explicit error status in streamed results', async t => {
  let changes = 0
  const app = setup({ followQueue: false, normalizeAttachments: value => value,
    onModuleToolChanged: () => changes++ })
  t.after(app.abortStream)
  t.mock.method(globalThis, 'fetch', async () => sse([
    ['tool_call', { seq: 1, id: 'todo', name: 'mcp__example__request', arguments: {} }],
    ['tool_result', { seq: 2, id: 'todo', name: 'mcp__example__request', result: { error: 'unavailable' }, is_error: true }],
    ['tool_call', { seq: 3, id: 'child', name: 'handoff_to_weekly', arguments: {} }],
    ['tool_result', { seq: 4, id: 'child', name: 'handoff_to_weekly', result: { status: 'failed' }, is_error: true }],
    ['assistant_done', { seq: 5 }],
  ]))
  await app.resumeJob('job-1', 1)
  assert.deepEqual(app.messages.value.map(message => message.status), ['error', 'error'])
  assert.equal(changes, 0)
})

test('retrying a lost response reconciles a message already loaded from history', async (t) => {
  const app = setup()
  let calls = 0
  const keys = []
  t.mock.method(globalThis, 'fetch', async (_url, options) => {
    keys.push(options.headers['Idempotency-Key'])
    if (++calls === 1) throw new Error('response lost after commit')
    return { ok: true, json: async () => ({ message: { id: 10 }, status: 'completed', job_id: 'job-1' }) }
  })
  app.input.value = 'hello'
  await app.sendMessage()
  const pending = app.messages.value[0]
  app.messages.value = [
    { role: 'user', id: 10, messageId: 10, content: 'hello', generationJobId: 'job-1', processingStatus: 'running' },
    { role: 'assistant', content: 'finished reply', jobId: 'job-1' },
    pending,
  ]
  const historyBubble = app.messages.value[0]
  await app.retryMessage(pending)
  assert.equal(keys[0], keys[1])
  assert.equal(app.messages.value.length, 2)
  assert.equal(app.messages.value[0], historyBubble)
  assert.equal(historyBubble.processingStatus, 'completed')
  assert.equal(app.messages.value[1].content, 'finished reply')
  assert.deepEqual(app.getLocalSubmissions(1), [])
})

test('reconciling a retry while streaming preserves the active reply', async (t) => {
  const app = setup()
  t.after(() => app.abortStream())
  const encoder = new TextEncoder()
  let controller
  let submissions = 0
  t.mock.method(globalThis, 'fetch', async (url) => {
    if (url.includes('/stream?')) return {
      ok: true, body: new ReadableStream({ start(value) { controller = value } }),
    }
    if (++submissions === 1) throw new Error('response lost')
    return { ok: true, json: async () => ({ message: { id: 10 }, status: 'running', job_id: 'job-1' }) }
  })
  app.input.value = 'hello'
  await app.sendMessage()
  const pending = app.messages.value[0]
  app.messages.value = [
    { role: 'user', id: 10, generationJobId: 'job-1', processingStatus: 'running' }, pending,
  ]
  const streaming = app.resumeJob('job-1', 1)
  await new Promise(resolve => setImmediate(resolve))
  controller.enqueue(encoder.encode('event: assistant_delta\ndata: {"seq":1,"delta":"first "}\n\n'))
  await new Promise(resolve => setImmediate(resolve))
  await app.retryMessage(pending)
  controller.enqueue(encoder.encode('event: assistant_delta\ndata: {"seq":2,"delta":"second"}\n\nevent: assistant_done\ndata: {"seq":3}\n\n'))
  controller.close()
  await streaming
  assert.equal(app.messages.value.length, 2)
  assert.equal(app.messages.value[1].content, 'first second')
  assert.equal(app.messages.value[0].processingStatus, 'completed')
  app.abortStream()
})

test('failed sends keep files and retry the same bubble, without duplicate submissions', async (t) => {
  const app = setup()
  const requests = []
  let finish
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    requests.push({ url, body: options.body, key: options.headers['Idempotency-Key'] })
    if (requests.length === 1) throw new Error('offline')
    await new Promise(resolve => { finish = resolve })
    return { ok: true, json: async () => ({ message: { id: 10 }, status: 'queued', job_id: 'job-1' }) }
  })
  app.input.value = 'hello'
  app.chatAttachments.value = [{ file: new File(['data'], 'a.txt'), name: 'a.txt' }]
  await app.sendMessage()
  const failed = app.messages.value[0]
  assert.equal(failed.processingStatus, 'failed')
  assert.equal(failed.processingError, 'offline')
  assert.equal(app.input.value, '')
  assert.equal(app.getLocalSubmissions(1)[0], failed)
  app.activeConversationId.value = 2
  await app.retryMessage(failed)
  assert.equal(requests.length, 1)
  app.activeConversationId.value = 1
  const retry = app.retryMessage(failed)
  await new Promise(resolve => setImmediate(resolve))
  await app.retryMessage(failed)
  assert.equal(requests.length, 2)
  assert.match(requests[0].key, /^[0-9a-f-]{36}$/)
  assert.equal(requests[1].key, requests[0].key)
  assert.equal(await requests[1].body.get('attachments[]').text(), 'data')
  finish()
  await retry
  assert.equal(app.messages.value.length, 1)
  assert.equal(failed.processingStatus, 'queued')
  assert.equal(failed.retryAttachments, undefined)
  assert.deepEqual(app.getLocalSubmissions(1), [])
  app.input.value = 'another message'
  finish = undefined
  const next = app.sendMessage()
  await new Promise(resolve => setImmediate(resolve))
  assert.notEqual(requests[2].key, requests[0].key)
  finish()
  await next
})

test('successful external tool results refresh module indicators, failed calls do not', async t => {
  let changes = 0
  const app = setup({ onModuleToolChanged: () => { changes += 1 }, normalizeAttachments: value => value })
  const encoder = new TextEncoder()
  t.mock.method(globalThis, 'fetch', async () => ({
    ok: true,
    body: new ReadableStream({ start(controller) {
      for (const [index, failed] of [false, true].entries()) {
        controller.enqueue(encoder.encode(`event: tool_result\ndata: ${JSON.stringify({ seq: index + 1, id: `todo-${index}`, name: 'mcp__example__request', result: { id: 1 }, is_error: failed })}\n\n`))
      }
      controller.close()
    } }),
  }))
  await app.resumeJob('job-todos', 1)
  assert.equal(changes, 1)
  app.abortStream()
})

test('loading or read-only conversations cannot send or clear their drafts', async (t) => {
  const app = setup()
  const fetch = t.mock.method(globalThis, 'fetch', async () => { throw new Error('unexpected request') })
  app.allowed.value = false
  app.input.value = 'draft'
  await app.sendMessage()
  assert.equal(fetch.mock.callCount(), 0)
  assert.equal(app.input.value, 'draft')
  assert.equal(app.messages.value.length, 0)
})

test('workspace progress updates the matching conversation and actual cwd', async t => {
  const patches = []
  const app = setup({ patchConversation: (id, patch) => patches.push({ id, ...patch }) })
  t.mock.method(globalThis, 'fetch', async () => ({ ok: true, body: new ReadableStream({ start(controller) {
    controller.enqueue(new TextEncoder().encode('event: workspace_progress\ndata: {"seq":1,"conversation_id":1,"workspace_state":"ready","cwd":".assistant-parallel/a/work"}\n\n'))
    controller.close()
  } }) }))
  await app.resumeJob('workspace-job', 1)
  assert.ok(patches.some(patch => patch.id === 1 && patch.workspace_state === 'ready' && patch.cwd === '.assistant-parallel/a/work'))
  app.abortStream()
})

test('command results restore input even when the start event is missing', async t => {
  const app = setup({
    normalizeAttachments: value => value,
    extractEchartsFromTool: () => null,
  })
  const input = { command: 'pwd', cwd: '/workspace' }
  const result = { output: '/workspace\n', exit_code: 0 }
  const events = [
    ['tool_call', { id: 'started', name: 'command', arguments: input }],
    ['tool_result', { id: 'started', name: 'command', result }],
    ['tool_result', { id: 'missing-start', name: 'command', arguments: input, result }],
  ]
  t.mock.method(globalThis, 'fetch', async () => ({
    ok: true,
    body: new ReadableStream({ start(controller) {
      for (const [index, [type, payload]] of events.entries()) {
        controller.enqueue(new TextEncoder().encode(
          `event: ${type}\ndata: ${JSON.stringify({ seq: index + 1, ...payload })}\n\n`,
        ))
      }
      controller.close()
    } }),
  }))
  await app.resumeJob('job-command', 1)
  assert.equal(app.messages.value.length, 2)
  for (const message of app.messages.value) {
    assert.deepEqual(message.input, input)
    assert.deepEqual(message.output, result)
    assert.equal(message.status, 'completed')
  }
  app.abortStream()
})

test('a send that fails after switching stays attached to its original conversation', async (t) => {
  const app = setup()
  let fail
  t.mock.method(globalThis, 'fetch', () => new Promise((_, reject) => { fail = reject }))
  app.input.value = 'from A'
  const pending = app.sendMessage()
  await new Promise(resolve => setImmediate(resolve))
  app.activeConversationId.value = 2
  app.messages.value = []
  fail(new Error('offline'))
  await pending
  assert.equal(app.messages.value.length, 0)
  assert.equal(app.getLocalSubmissions(1)[0].processingStatus, 'failed')
  assert.deepEqual(app.getLocalSubmissions(2), [])
})

test('resuming a job preserves its original elapsed-time baseline', async (t) => {
  const app = setup()
  const submittedAt = '2026-09-10T01:02:00.000Z'
  const startedAt = '2026-09-10T01:02:03.000Z'
  app.messages.value = [
    {
      role: 'user',
      content: 'hello',
      jobId: 'job-1',
      generationJobId: 'job-1',
      time: submittedAt,
    },
    {
      role: 'tool',
      kind: 'tool-run',
      jobId: 'job-1',
      id: 'old-tool',
      status: 'running',
      startedAt,
    },
  ]
  const encoder = new TextEncoder()
  t.mock.method(globalThis, 'fetch', async () => ({
    ok: true,
    body: new ReadableStream({
      start(controller) {
        controller.enqueue(encoder.encode(
          'event: tool_call\ndata: {"seq":1,"id":"tool-1","name":"search","arguments":{}}\n\n',
        ))
        controller.close()
      },
    }),
  }))

  assert.equal(getJobStartedAt(app.messages.value, 'job-1'), startedAt)
  await app.resumeJob('job-1', 1)

  const replayedTool = app.messages.value.find((message) => message.id === 'tool-1')
  assert.equal(replayedTool.startedAt, startedAt)
  assert.equal(app.messages.value.some((message) => message.id === 'old-tool'), false)
  app.abortStream()
})

test('replayed events preserve server timing and a finished reply stops its clock even hours later', async t => {
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse('2026-10-12T05:00:00Z') })
  const app = setup({ followQueue: false, normalizeAttachments: attachments => attachments || [] })
  t.after(app.abortStream)
  app.messages.value = [{ role: 'user', generationJobId: 'job-1', processingStatus: 'queued', time: '2026-10-12T00:00:00Z' }]
  t.mock.method(globalThis, 'fetch', async () => sse([
    ['stream_started', { job_id: 'job-1', started_at: '2026-10-12T01:00:00Z' }],
    ['tool_call', { seq: 1, id: 'command', name: 'command', occurred_at: '2026-10-12T01:01:00Z' }],
    ['tool_result', { seq: 2, id: 'command', name: 'command', occurred_at: '2026-10-12T01:02:00Z' }],
    ['assistant_delta', { seq: 3, delta: 'reply', occurred_at: '2026-10-12T01:03:00Z' }],
    ['assistant_delta', { seq: 4, delta: ' finished', occurred_at: '2026-10-12T01:04:00Z' }],
    ['assistant_done', { seq: 5, occurred_at: '2026-10-12T01:05:00Z' }],
  ]))
  await app.resumeJob('job-1', 1)
  const tool = app.messages.value.find(message => message.id === 'command')
  assert.equal(tool.startedAt, '2026-10-12T01:01:00Z')
  assert.equal(tool.completedAt, '2026-10-12T01:02:00Z')
  assert.ok(app.messages.value.every(message => message.jobStartedAt === '2026-10-12T01:00:00Z'))
  assert.ok(app.messages.value.every(message => message.jobCompletedAt === '2026-10-12T01:05:00Z'))
  const timeline = useMessageTimeline({ messages: app.messages, activeConversationId: app.activeConversationId })
  assert.equal(timeline.groupedMessages.value.find(item => item.msg?.role === 'user').timeValue, '2026-10-12T01:00:00.000Z')
  const process = timeline.groupedMessages.value.find(item => item.type === 'process')
  assert.equal(process.running, false)
  assert.equal(getProcessDurationMs(process.timingSteps, process.running), 300000)
})

test('failed and cancelled replies freeze at their terminal server time', async t => {
  for (const [type, payload, status] of [
    ['error', { message: 'upstream failed' }, 'failed'],
    ['assistant_done', { cancelled: true }, 'cancelled'],
  ]) {
    await t.test(status, async child => {
      const app = setup({ followQueue: false })
      child.after(app.abortStream)
      child.mock.method(globalThis, 'fetch', async () => sse([
        ['stream_started', { job_id: 'job-1', started_at: '2026-10-12T01:00:00Z' }],
        ['tool_call', { seq: 1, id: 'command', name: 'command', occurred_at: '2026-10-12T01:01:00Z' }],
        [type, { seq: 2, occurred_at: '2026-10-12T01:02:00Z', ...payload }],
        ...(type === 'error' ? [['assistant_done', { seq: 3, occurred_at: '2026-10-12T01:02:00Z' }]] : []),
      ]))
      await app.resumeJob('job-1', 1)
      assert.ok(app.messages.value.every(message => message.jobStatus === status))
      const timeline = useMessageTimeline({ messages: app.messages, activeConversationId: app.activeConversationId })
      const process = timeline.groupedMessages.value.find(item => item.type === 'process')
      assert.equal(process.running, false)
      assert.equal(getProcessDurationMs(process.timingSteps, process.running, Date.parse('2026-10-12T05:00:00Z')), 120000)
    })
  }
})

test('independent Firefly turns stream concurrently without following another queued turn', async t => {
  const first = setup({ followQueue: false })
  const second = setup({ followQueue: false })
  second.activeConversationId.value = 2
  const requests = []
  t.mock.method(globalThis, 'fetch', async url => {
    requests.push(url)
    assert.match(url, /\/jobs\/job-[12]\/stream/)
    const text = url.includes('job-1') ? 'first reply' : 'second reply'
    return { ok: true, body: new ReadableStream({ start(controller) {
      controller.enqueue(new TextEncoder().encode(
        `event: assistant_delta\ndata: ${JSON.stringify({ seq: 1, delta: text })}\n\nevent: assistant_done\ndata: {"seq":2}\n\n`,
      ))
      controller.close()
    } }) }
  })
  await Promise.all([first.resumeJob('job-1', 1), second.resumeJob('job-2', 2)])
  assert.equal(first.messages.value[0].content, 'first reply')
  assert.equal(second.messages.value[0].content, 'second reply')
  assert.equal(requests.length, 2)
  first.abortStream()
  second.abortStream()
})

for (const titleArrivesInStream of [false, true]) {
  test(`conversation title ${titleArrivesInStream ? 'updates during streaming' : 'does not poll after a quick reply'}`, async t => {
    t.mock.timers.enable({ apis: ['setTimeout'] })
    const patches = []
    const refreshes = []
    const app = setup({
      followQueue: false,
      patchConversation: (id, patch) => { patches.push({ id, ...patch }); return true },
      fetchConversations: options => refreshes.push(options),
    })
    t.mock.method(globalThis, 'fetch', async () => ({
      ok: true,
      body: new ReadableStream({ start(controller) {
        const title = titleArrivesInStream
          ? 'event: conversation_title\ndata: {"seq":1,"conversation_id":1,"title":"修复登录页面加载问题"}\n\n'
          : ''
        controller.enqueue(new TextEncoder().encode(`${title}event: assistant_done\ndata: {"seq":2}\n\n`))
        controller.close()
      } }),
    }))
    await app.resumeJob('title-job', 1)
    assert.equal(refreshes.length, 1)
    if (titleArrivesInStream) {
      assert.ok(patches.some(patch => patch.id === 1 && patch.title === '修复登录页面加载问题'))
    }
    t.mock.timers.tick(12000)
    assert.equal(refreshes.length, 1)
    app.abortStream()
  })
}

test('failed lazy creation preserves the first message and attachments for retry', async t => {
  let fail = true
  const app = setup({ ensureConversation: async () => {
    if (fail) throw new Error('creation unavailable')
    app.activeConversationId.value = 7
    return { id: 7 }
  } })
  t.after(app.abortStream)
  app.activeConversationId.value = null
  app.input.value = 'first message'
  app.chatAttachments.value = [{ file: new File(['data'], 'notes.txt'), name: 'notes.txt' }]
  await app.sendMessage()
  assert.equal(app.input.value, 'first message')
  assert.equal(app.chatAttachments.value.length, 1)
  assert.match(app.conversationError.value, /creation unavailable/)
  fail = false
  const submissions = []
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    submissions.push({ url, message: options.body.get('message') })
    return { ok: true, json: async () => ({ status: 'completed', message: { id: 1 } }) }
  })
  await app.sendMessage()
  assert.equal(app.input.value, '')
  assert.equal(app.chatAttachments.value.length, 0)
  assert.deepEqual(submissions, [{ url: '/api/assistant/conversations/7/messages', message: 'first message' }])
})


test('switching conversations stops process recovery and ignores late socket snapshots', async t => {
  const app = setupRealtime(t, { transformServerMessages: transformSnapshot })
  t.mock.method(globalThis, 'fetch', async url => url.includes('/stream?') ? { ok: false, status: 404 }
    : { ok: true, json: async () => ({ messages: [snapshotMessage(1, 'user', 'question', 'running')] }) })
  await app.resumeJob('job-1', 1)
  const subscription = app.subscriptions[0]
  app.activeConversationId.value = 2
  app.messages.value = [{ role: 'assistant', content: 'another conversation' }]
  assert.equal(subscription.stopped, true)
  subscription.receive({ messages: [snapshotMessage(2, 'assistant', 'stale reply')] })
  assert.equal(app.messages.value[0].content, 'another conversation')
  assert.equal(app.isStreaming.value, false)
})

test('failed initial recovery follows socket updates without retrying process HTTP requests', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  const app = setupRealtime(t, { transformServerMessages: transformSnapshot })
  const requests = []
  t.mock.method(globalThis, 'fetch', async url => {
    requests.push(url)
    return { ok: false, status: url.includes('/stream?') ? 410 : 503 }
  })
  await app.resumeJob('job-1', 1)
  assert.ok(app.conversationError.value)
  app.subscriptions[0].receive({ messages: [snapshotMessage(1, 'user', 'question'), snapshotMessage(2, 'assistant', 'saved answer')] })
  assert.equal(app.conversationError.value, '')
  assert.equal(app.messages.value.at(-1).content, 'saved answer')
  assert.equal(app.isStreaming.value, false)
  t.mock.timers.tick(300000)
  assert.equal(requests.length, 2)
})

test('summary-only tool events update status without populating input or output', async t => {
  const app = setup({ followQueue: false, normalizeAttachments: files => files || [] })
  t.after(app.abortStream)
  const events = [
    ['tool_call', { id: 'command', name: 'command', detail_omitted: true }],
    ['tool_result', { id: 'command', name: 'command', is_error: false, detail_omitted: true }],
    ['tool_call', { id: 'handoff', name: 'handoff_to_programmer', detail_omitted: true }],
    ['handoff_started', { id: 'handoff', agent: 'programmer' }],
    ['tool_call', { id: 'child', name: 'run_tools', agent: 'programmer', parent_id: 'handoff', detail_omitted: true }],
    ['tool_result', { id: 'child', name: 'run_tools', agent: 'programmer', parent_id: 'handoff', is_error: false, detail_omitted: true }],
    ['tool_result', { id: 'handoff', name: 'handoff_to_programmer', is_error: false, detail_omitted: true }],
    ['assistant_done', {}],
  ]
  t.mock.method(globalThis, 'fetch', async () => ({ ok: true, body: new ReadableStream({ start(controller) {
    events.forEach(([type, payload], index) => controller.enqueue(new TextEncoder().encode(
      `event: ${type}\ndata: ${JSON.stringify({ seq: index + 1, ...payload })}\n\n`,
    )))
    controller.close()
  } }) }))
  await app.resumeJob('job-summary', 1)
  const root = app.messages.value.find(message => message.id === 'command')
  const handoff = app.messages.value.find(message => message.id === 'handoff')
  for (const step of [root, handoff, handoff.tools[0]]) {
    assert.equal(step.status, 'completed')
    assert.ok(step.input == null)
    assert.ok(step.output == null)
  }
  app.abortStream()
})
