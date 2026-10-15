import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createRenderer, h, nextTick, ref, ssrContextKey } from 'vue'
import { createServer } from 'vite'
import { getProcessDurationMs } from './processDuration.js'

const server = await createServer({ configFile: 'frontend/vite.config.ts', server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { useMessageTimeline } = await server.ssrLoadModule('/src/composables/useMessageTimeline.js')
const { provideAssistantRealtime } = await server.ssrLoadModule('/src/composables/useAssistantRealtime.js')
const { default: ProcessGroup } = await server.ssrLoadModule('/src/components/ProcessGroup.vue')

test('only the latest reply stays outside the process through streaming, completion and reload', async () => {
  const conversationId = ref(1)
  const messages = ref([
    { role: 'user', generationJobId: 'live', content: 'question', processingStatus: 'running' },
    { role: 'assistant', jobId: 'live', content: 'visible progress note' },
  ])
  const timeline = useMessageTimeline({ messages, activeConversationId: conversationId })
  const replies = () => timeline.groupedMessages.value.filter(item => item.type === 'msg' && item.msg.role === 'assistant')
  const originalKey = timeline.groupedMessages.value[1].key
  messages.value.push({ role: 'tool', kind: 'tool-run', jobId: 'live', id: 'call', status: 'running' })
  assert.deepEqual(timeline.groupedMessages.value.map(item => item.type), ['msg', 'process', 'msg'])
  assert.equal(replies()[0].key, originalKey)
  const originalMessageKey = replies()[0].messageKey
  messages.value.push({ role: 'assistant', jobId: 'live', content: 'new reply' })
  assert.deepEqual(timeline.groupedMessages.value.map(item => item.type), ['msg', 'process', 'msg'])
  assert.deepEqual(replies().map(item => item.msg.content), ['new reply'])
  assert.equal(replies()[0].key, originalKey)
  assert.notEqual(replies()[0].messageKey, originalMessageKey)
  assert.equal(timeline.groupedMessages.value[1].steps[0].content, 'visible progress note')
  const replyKey = replies()[0].key
  messages.value.at(-1).content += ' with more text'
  messages.value.push({ role: 'assistant', jobId: 'live', content: ' \n ' })
  assert.equal(replies()[0].key, replyKey)
  assert.deepEqual(replies().map(item => item.msg.content), ['new reply with more text'])
  messages.value[0].processingStatus = 'completed'
  messages.value = messages.value.map(message => ({ ...message, status: 'completed' }))
  assert.equal(replies()[0].key, replyKey)
  assert.deepEqual(timeline.groupedMessages.value.map(item => item.type), ['msg', 'process', 'msg'])
  assert.deepEqual(replies().map(item => item.msg.content), ['new reply with more text'])
  assert.equal(messages.value[1].content, 'visible progress note')
  conversationId.value = 2
  await nextTick()
  assert.deepEqual(timeline.groupedMessages.value.map(item => item.type), ['msg', 'process', 'msg'])
  assert.deepEqual(replies().map(item => item.msg.content), ['new reply with more text'])
})

test('consecutive replies fold into each turn’s process without hiding result images or charts', () => {
  const messages = ref([])
  for (const jobId of ['job', undefined]) {
    messages.value.push(
      { role: 'user', generationJobId: jobId, content: 'question' },
      { role: 'assistant', jobId, content: 'first note' },
      { role: 'assistant', jobId, content: 'second note' },
      { role: 'assistant', jobId, chartOption: {} },
      { role: 'assistant', jobId, kind: 'image-result', imageOutput: { type: 'image', image_url: '/image.png' } },
      { role: 'assistant', jobId, content: 'latest reply' },
    )
  }
  const timeline = useMessageTimeline({ messages, activeConversationId: ref(1) })
  const visible = timeline.groupedMessages.value.filter(item => item.type === 'msg' && item.msg.role === 'assistant')
  assert.deepEqual(visible.filter(item => item.msg.content).map(item => item.msg.content), ['latest reply', 'latest reply'])
  assert.equal(visible.filter(item => item.msg.chartOption).length, 2)
  assert.equal(visible.filter(item => item.msg.imageOutput).length, 2)
  const processes = timeline.groupedMessages.value.filter(item => item.type === 'process')
  assert.equal(processes.length, 2)
  for (const process of processes) {
    assert.deepEqual(process.steps.map(step => step.content), ['first note', 'second note'])
  }
})

test('streaming regrouping and snapshot replacement preserve the user’s process expansion choice', async t => {
  const messages = ref([
    { role: 'user', generationJobId: 'job', content: 'question' },
    { role: 'assistant', jobId: 'job', content: 'progress note' },
    { role: 'tool', kind: 'tool-run', id: 'call', jobId: 'job', status: 'running' },
  ])
  const conversationId = ref(1)
  let timeline, state, mounts = 0
  // Mount the actual process setup with keyed children, without requiring a DOM.
  const Process = { ...ProcessGroup, setup(props, context) {
    mounts++
    state = ProcessGroup.setup(props, context)
    return state
  }, render: () => null }
  const { createApp } = createRenderer({ createComment: () => ({}), createElement: () => ({}),
    insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = createApp({ setup() {
    timeline = useMessageTimeline({ messages, activeConversationId: conversationId })
    return () => h('div', timeline.groupedMessages.value.filter(item => item.type === 'process').map(item => h(Process, {
      key: item.key, steps: item.steps, running: item.running,
      toolStatusText: String, formatToolValue: String, renderMessageHtml: () => '',
    })))
  } })
  app.provide(ssrContextKey, { modules: new Set() })
  app.mount({})
  t.after(() => app.unmount())
  assert.equal(state.open.value, false)
  state.toggle()
  assert.equal(state.open.value, true)
  const originalKey = timeline.groupedMessages.value.find(item => item.type === 'process').key

  messages.value.push({ role: 'assistant', jobId: 'job', content: 'new reply' })
  await nextTick()
  const regrouped = timeline.groupedMessages.value.find(item => item.type === 'process')
  assert.equal(regrouped.steps[0].content, 'progress note')
  assert.equal(regrouped.key, originalKey)
  assert.equal(state.open.value, true)
  assert.equal(mounts, 1)

  messages.value = messages.value.map(message => ({ ...message, status: 'completed' }))
  await nextTick()
  assert.equal(state.open.value, true)
  assert.equal(mounts, 1)

  state.toggle()
  messages.value.push({ role: 'tool', kind: 'tool-run', id: 'next-call', jobId: 'job', status: 'running' })
  messages.value.push({ role: 'assistant', jobId: 'job', content: 'final reply' })
  await nextTick()
  assert.equal(state.open.value, false)
  assert.equal(mounts, 1)

  state.toggle()
  conversationId.value = 2
  await nextTick()
  assert.equal(state.open.value, false)
  assert.equal(mounts, 2)
})

test('separate process blocks of the same job have distinct stable keys', () => {
  const messages = ref([
    { role: 'tool', kind: 'tool-run', id: 'first', jobId: 'job' },
    { role: 'assistant', chartOption: {}, jobId: 'job' },
    { role: 'tool', kind: 'tool-run', id: 'second', jobId: 'job' },
  ])
  const timeline = useMessageTimeline({ messages, activeConversationId: ref(1) })
  const keys = () => timeline.groupedMessages.value.filter(item => item.type === 'process').map(item => item.key)
  const original = keys()
  assert.equal(new Set(original).size, 2)
  messages.value = messages.value.map(message => ({ ...message }))
  assert.deepEqual(keys(), original)
})

test('timeline keeps turn boundaries, terminal replies and stable message keys', () => {
  const messages = ref([
    { role: 'user', jobId: 'job', content: 'question', time: '2026-09-11T01:00:00Z' },
    { role: 'assistant', jobId: 'job', content: 'thinking' },
    { role: 'tool', content: 'result', jobId: 'job' },
    { role: 'assistant', jobId: 'job', content: 'answer' },
    { role: 'user', content: 'next question', time: '2026-09-11T01:10:00Z' },
  ])
  const timeline = useMessageTimeline({ messages, activeConversationId: ref(1) })
  const first = timeline.groupedMessages.value
  assert.deepEqual(first.map(item => item.type), ['msg', 'process', 'msg', 'msg'])
  assert.equal(first[1].steps.length, 2)
  assert.equal(first[2].msg.content, 'answer')
  assert.ok(first[3].timeLabel)
  messages.value[3].content += ' updated'
  assert.equal(timeline.groupedMessages.value[2].key, first[2].key)
})

test('every user message keeps its send time while assistant replies and process blocks hide theirs', t => {
  t.mock.timers.enable({ apis: ['Date'], now: new Date('2026-10-12T14:00:00').getTime() })
  const messages = ref([
    { role: 'user', jobId: 'job', content: 'question', time: '2026-10-12T12:12:00' },
    { role: 'tool', jobId: 'job', content: 'working', time: '2026-10-12T12:12:01' },
    { role: 'assistant', jobId: 'job', content: 'answer', time: '2026-10-12T13:12:00' },
    { role: 'tool', jobId: 'job', content: 'cleanup', time: '2026-10-12T13:12:05' },
    { role: 'user', content: 'next question', time: '2026-10-12T13:12:30' },
  ])
  const timeline = useMessageTimeline({ messages, activeConversationId: ref(1) })
  const items = timeline.groupedMessages.value
  assert.deepEqual(items.map(item => item.type), ['msg', 'process', 'msg', 'msg'])
  assert.deepEqual(items.map(item => item.timeLabel), ['12:12', '', '', '13:12'])
  const visible = items.filter(item => item.type === 'msg' && item.msg.role === 'user')
  assert.deepEqual(visible.map(item => item.timeValue), visible.map(item => new Date(item.msg.time).toISOString()))
  assert.match(visible.at(-1).timeTitle, /13:12:30/)
  for (const item of items.slice(1, 3)) {
    assert.equal(item.timeValue, '')
    assert.equal(item.timeTitle, '')
  }
  messages.value[2].content += ' streamed text'
  assert.deepEqual(timeline.groupedMessages.value.map(item => item.timeValue), items.map(item => item.timeValue))
})

test('message times distinguish historical dates and omit missing or invalid timestamps', t => {
  t.mock.timers.enable({ apis: ['Date'], now: new Date('2026-10-12T14:00:00').getTime() })
  const timeline = useMessageTimeline({ activeConversationId: ref(1), messages: ref([
    { role: 'user', time: '2025-10-10T09:05:00' },
    { role: 'user', time: '2026-10-10T09:05:00' },
    { role: 'user', time: '2026-10-11T09:05:00' },
    { role: 'user', time: '2026-10-12T09:05:00' },
    { role: 'user' },
    { role: 'user', time: 'invalid' },
    { role: 'assistant', content: 'answer', time: '2026-10-12T09:05:30' },
  ]) })
  const items = timeline.groupedMessages.value
  assert.deepEqual(items.map(item => item.timeLabel), [
    '2025年10月10日 09:05', '10月10日 09:05', '昨天 09:05', '09:05', '', '', '',
  ])
  for (const item of items.slice(-3)) {
    assert.equal(item.timeValue, '')
    assert.equal(item.timeTitle, '')
  }
})

test('process details are fetched once without changing message timestamps', async (t) => {
  let calls = 0
  t.mock.method(globalThis, 'fetch', async () => {
    calls++
    return { ok: true, json: async () => ({ messages: [{ role: 'tool', content: 'full details' }] }) }
  })
  const timeline = useMessageTimeline({
    activeConversationId: ref(1),
    messages: ref([
      { role: 'tool', jobId: 'job', detailOmitted: true, time: '2026-09-11T01:00:00Z' },
      { role: 'assistant', jobId: 'job', content: 'answer', time: '2026-09-11T02:00:00Z' },
      { role: 'user', content: 'next question', time: '2026-09-11T02:00:30Z' },
    ]),
    transformServerMessages: records => records,
  })
  const original = timeline.groupedMessages.value[0]
  const messageTimes = timeline.groupedMessages.value.slice(1).map(item => item.timeValue)
  await timeline.loadProcessDetail(original)
  const loaded = timeline.groupedMessages.value[0]
  // Vue must reuse the expanded component when summary steps are replaced by details.
  assert.equal(loaded.key, original.key)
  assert.equal(loaded.steps[0].content, 'full details')
  assert.equal(loaded.timeLabel, '')
  assert.deepEqual(timeline.groupedMessages.value.slice(1).map(item => item.timeValue), messageTimes)
  assert.equal(loaded.needsDetail, false)
  await timeline.loadProcessDetail(loaded)
  assert.equal(calls, 1)
})

test('elapsed time continues after tools finish until the whole reply finishes', () => {
  const messages = ref([
    { role: 'user', generationJobId: 'job', time: '2026-10-12T00:00:00Z', processingStatus: 'running',
      jobStartedAt: '2026-10-12T01:00:00Z' },
    { role: 'tool', jobId: 'job', status: 'completed', startedAt: '2026-10-12T01:01:00Z', completedAt: '2026-10-12T01:02:00Z' },
    { role: 'assistant', jobId: 'job', content: 'streaming reply', time: '2026-10-12T01:03:00Z' },
  ])
  const timeline = useMessageTimeline({ messages, activeConversationId: ref(1) })
  let process = timeline.groupedMessages.value.find(item => item.type === 'process')
  assert.equal(process.running, true)
  assert.equal(getProcessDurationMs(process.timingSteps, process.running, Date.parse('2026-10-12T01:04:00Z')), 240000)
  messages.value[0].processingStatus = 'completed'
  messages.value[0].jobCompletedAt = '2026-10-12T01:05:00Z'
  process = timeline.groupedMessages.value.find(item => item.type === 'process')
  assert.equal(process.running, false)
  assert.equal(getProcessDurationMs(process.timingSteps, process.running, Date.parse('2026-10-12T02:00:00Z')), 300000)
})

test('result-separated processes keep one clock while only the latest reply remains visible', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setInterval'], now: Date.parse('2026-10-12T01:07:55Z') })
  const messages = ref([
    { role: 'user', generationJobId: 'job', content: 'question', processingStatus: 'running',
      jobStartedAt: '2026-10-12T01:00:00Z' },
    { role: 'tool', kind: 'tool-run', id: 'first', jobId: 'job', status: 'completed' },
    { role: 'assistant', jobId: 'job', content: 'first progress note' },
    { role: 'assistant', jobId: 'job', chartOption: {} },
    { role: 'tool', kind: 'tool-run', id: 'second', jobId: 'job', status: 'completed' },
    { role: 'assistant', jobId: 'job', content: 'second progress note' },
    { role: 'assistant', jobId: 'job', chartOption: {} },
    { role: 'tool', kind: 'tool-run', id: 'third', jobId: 'job', status: 'running' },
    { role: 'assistant', jobId: 'job', content: 'latest progress note' },
  ])
  let timeline
  const states = new Map()
  const Process = { ...ProcessGroup, setup(props, context) {
    const state = ProcessGroup.setup(props, context)
    states.set(props.steps[0].id, state)
    return state
  }, render: () => null }
  const { createApp } = createRenderer({ createComment: () => ({}), createElement: () => ({}),
    insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = createApp({ setup() {
    timeline = useMessageTimeline({ messages, activeConversationId: ref(1) })
    return () => h('div', timeline.groupedMessages.value.filter(item => item.type === 'process').map(item => h(Process, {
      key: item.key, steps: item.steps, timingSteps: item.timingSteps,
      showDuration: item.showDuration, running: item.running,
      toolStatusText: String, formatToolValue: String, renderMessageHtml: () => '',
    })))
  } })
  app.provide(ssrContextKey, { modules: new Set() })
  app.mount({})
  t.after(() => app.unmount())
  const processes = () => timeline.groupedMessages.value.filter(item => item.type === 'process')
  const summaries = () => processes().map(item => states.get(item.steps[0].id).summaryText.value)
  const replies = () => timeline.groupedMessages.value.filter(item => item.type === 'msg' && item.msg.content
    && item.msg.role === 'assistant').map(item => item.msg.content)
  const originalKeys = processes().map(item => item.key)
  assert.deepEqual(summaries(), ['执行过程', '执行过程', '执行了 7m 55s'])
  assert.deepEqual(processes().map(item => item.running), [false, false, true])
  assert.deepEqual(replies(), ['latest progress note'])
  states.get('first').toggle()

  t.mock.timers.tick(5000)
  await nextTick()
  assert.deepEqual(summaries(), ['执行过程', '执行过程', '执行了 8m 0s'])
  messages.value.find(message => message.id === 'third').status = 'completed'
  messages.value.push(
    { role: 'assistant', jobId: 'job', chartOption: {} },
    { role: 'tool', kind: 'tool-run', id: 'fourth', jobId: 'job', status: 'running' },
    { role: 'assistant', jobId: 'job', content: 'newer progress note' },
  )
  await nextTick()
  assert.deepEqual(summaries(), ['执行过程', '执行过程', '执行过程', '执行了 8m 0s'])
  assert.deepEqual(processes().map(item => item.running), [false, false, false, true])
  assert.deepEqual(replies(), ['newer progress note'])
  assert.deepEqual(processes().slice(0, 3).map(item => item.key), originalKeys)
  assert.equal(states.get('first').open.value, true)

  messages.value[0].processingStatus = 'completed'
  messages.value[0].jobCompletedAt = '2026-10-12T01:08:05Z'
  messages.value.find(message => message.id === 'fourth').status = 'completed'
  messages.value.push({ role: 'assistant', jobId: 'job', content: 'final reply' })
  messages.value = messages.value.map(message => ({ ...message }))
  await nextTick()
  t.mock.timers.tick(60000)
  await nextTick()
  assert.deepEqual(summaries(), ['执行过程', '执行过程', '执行过程', '执行了 8m 5s'])
  assert.ok(processes().every(item => !item.running))
  assert.deepEqual(replies(), ['final reply'])
  assert.equal(states.get('first').open.value, true)
})

test('separate turns retain their own duration, including legacy turns without job IDs', () => {
  const messages = []
  for (const jobId of ['first-job', 'second-job', undefined, undefined]) {
    messages.push(
      { role: 'user', generationJobId: jobId, content: 'question' },
      { role: 'tool', jobId, status: 'completed', startedAt: '2026-10-12T01:00:00Z' },
      { role: 'assistant', jobId, chartOption: {} },
      { role: 'tool', jobId, status: 'completed', completedAt: '2026-10-12T01:01:00Z' },
      { role: 'assistant', jobId, content: 'final reply', time: '2026-10-12T01:04:00Z' },
    )
  }
  const timeline = useMessageTimeline({ messages: ref(messages), activeConversationId: ref(1) })
  const processes = timeline.groupedMessages.value.filter(item => item.type === 'process')
  assert.equal(processes.length, 8)
  assert.deepEqual(processes.map(item => item.showDuration), [false, true, false, true, false, true, false, true])
  for (const process of processes.filter(item => item.showDuration)) {
    assert.equal(getProcessDurationMs(process.timingSteps, process.running), 240000)
  }
})

test('legacy execution duration reaches the final reply instead of ending at the last tool', () => {
  const timeline = useMessageTimeline({ activeConversationId: ref(1), messages: ref([
    { role: 'user', time: '2026-10-12T00:00:00Z' },
    { role: 'tool', status: 'completed', startedAt: '2026-10-12T01:00:00Z', completedAt: '2026-10-12T01:01:00Z' },
    { role: 'assistant', content: 'final reply', time: '2026-10-12T01:04:00Z' },
  ]) })
  const process = timeline.groupedMessages.value.find(item => item.type === 'process')
  assert.equal(getProcessDurationMs(process.timingSteps, process.running), 240000)
})

test('a compact legacy summary loads its range only on demand and retains duration', async t => {
  const requests = []
  t.mock.method(globalThis, 'fetch', async url => {
    requests.push(new URL(url, 'http://localhost'))
    return { ok: true, json: async () => ({ messages: [{ role: 'tool', content: 'execution details' }] }) }
  })
  const timeline = useMessageTimeline({
    activeConversationId: ref(7),
    messages: ref([
      { role: 'user', content: 'question' },
      { role: 'tool', kind: 'process-summary', detailOmitted: true,
        startedAt: '2026-09-11T01:00:00Z', completedAt: '2026-09-11T01:00:42Z',
        processRange: { start_id: 2, end_id: 200 } },
      { role: 'assistant', content: 'answer' },
    ]),
    transformServerMessages: records => records,
  })
  const summary = timeline.groupedMessages.value[1]
  assert.equal(requests.length, 0)
  assert.equal(summary.needsDetail, true)
  assert.equal(getProcessDurationMs(summary.timingSteps, false), 42000)
  await timeline.loadProcessDetail(summary)
  assert.equal(requests.length, 1)
  assert.equal(requests[0].pathname, '/api/assistant/conversations/7/process')
  assert.equal(requests[0].searchParams.get('start_id'), '2')
  assert.equal(requests[0].searchParams.get('end_id'), '200')
  assert.equal(requests[0].searchParams.has('job_id'), false)
  const details = timeline.groupedMessages.value[1]
  assert.equal(details.key, summary.key)
  assert.equal(getProcessDurationMs(details.timingSteps, false), 42000)
  assert.equal(details.steps[0].content, 'execution details')
  await timeline.loadProcessDetail(summary)
  assert.equal(requests.length, 1)
})

test('details can be retried, deduplicate concurrent requests, and cache empty results', async t => {
  let finish
  let calls = 0
  t.mock.method(globalThis, 'fetch', async () => {
    calls++
    if (calls === 1) return new Promise(resolve => { finish = resolve })
    return { ok: true, json: async () => ({ messages: [] }) }
  })
  const timeline = useMessageTimeline({
    activeConversationId: ref(1),
    messages: ref([{ role: 'tool', jobId: 'job', detailOmitted: true }]),
    transformServerMessages: records => records,
  })
  const item = timeline.groupedMessages.value[0]
  const pending = timeline.loadProcessDetail(item)
  await timeline.loadProcessDetail(item)
  assert.equal(calls, 1)
  finish({ ok: false, text: async () => 'offline' })
  await pending
  assert.equal(timeline.processDetailErrors.get(item.detailKey), 'offline')
  await timeline.loadProcessDetail(item)
  assert.equal(timeline.processDetailErrors.has(item.detailKey), false)
  assert.equal(timeline.groupedMessages.value[0].needsDetail, false)
  await timeline.loadProcessDetail(item)
  assert.equal(calls, 2)
})

test('separate ranges of the same job do not reuse another range’s details', async t => {
  const urls = []
  t.mock.method(globalThis, 'fetch', async url => {
    urls.push(url)
    return { ok: true, json: async () => ({ messages: [{ role: 'tool', content: url }] }) }
  })
  const summary = (start, end) => ({ role: 'tool', jobId: 'job', detailOmitted: true, processRange: { start_id: start, end_id: end } })
  const timeline = useMessageTimeline({
    activeConversationId: ref(1),
    messages: ref([summary(2, 3), { role: 'assistant', chartOption: {}, jobId: 'job' }, summary(5, 6)]),
    transformServerMessages: records => records,
  })
  const items = timeline.groupedMessages.value.filter(item => item.type === 'process')
  assert.equal(items.length, 2)
  await timeline.loadProcessDetail(items[0])
  await timeline.loadProcessDetail(items[1])
  assert.equal(urls.length, 2)
  assert.ok(urls[0].includes('start_id=2'))
  assert.ok(urls[1].includes('start_id=5'))
})

test('the unified feed keeps queued questions visible', () => {
  const timeline = useMessageTimeline({
    activeConversationId: ref(1), includeQueued: true,
    messages: ref([{ role: 'user', processingStatus: 'queued', content: 'waiting question' }]),
  })
  assert.equal(timeline.groupedMessages.value[0].msg.content, 'waiting question')
})

test('a tool call after the final reply is folded so the reply stays last outside', () => {
  const timeline = useMessageTimeline({
    activeConversationId: ref(1),
    messages: ref([
      { role: 'user', jobId: 'job', content: 'question' },
      { role: 'tool', kind: 'tool-run', jobId: 'job', name: 'command', status: 'completed' },
      { role: 'assistant', jobId: 'job', content: 'progress note' },
      { role: 'tool', kind: 'tool-run', jobId: 'job', name: 'command', status: 'completed' },
      { role: 'assistant', jobId: 'job', content: 'final answer' },
      { role: 'tool', kind: 'tool-run', jobId: 'job', name: 'command', status: 'completed' },
    ]),
  })
  const items = timeline.groupedMessages.value
  assert.deepEqual(items.map(item => item.type), ['msg', 'process', 'msg'])
  assert.equal(items[1].steps.length, 4)
  assert.equal(items[2].msg.content, 'final answer')
})

test('result messages after the final reply text keep their order outside', () => {
  const timeline = useMessageTimeline({
    activeConversationId: ref(1),
    messages: ref([
      { role: 'user', jobId: 'job', content: 'question' },
      { role: 'assistant', jobId: 'job', content: 'final answer' },
      { role: 'assistant', jobId: 'job', chartOption: { series: [] } },
    ]),
  })
  const items = timeline.groupedMessages.value
  assert.deepEqual(items.map(item => item.type), ['msg', 'msg', 'msg'])
  assert.equal(items[1].msg.content, 'final answer')
  assert.ok(items[2].msg.chartOption)
})

test('reloaded final reply stays outside merged process details with delayed tool results', async t => {
  const reply = { role: 'assistant', jobId: 'job', messageId: 4, content: 'final answer' }
  const summary = (start, end) => ({ role: 'tool', kind: 'process-summary', jobId: 'job',
    detailOmitted: true, processRange: { start_id: start, end_id: end } })
  let requests = 0
  t.mock.method(globalThis, 'fetch', async () => {
    requests++
    return { ok: true, json: async () => ({ messages: [
      { id: 2, role: 'assistant', content: 'progress note' },
      { id: 3, role: 'tool', content: 'first tool result' },
      { id: 4, role: 'assistant', content: 'final answer' },
      { id: 5, role: 'tool', content: 'delayed tool result' },
    ] }) }
  })
  const timeline = useMessageTimeline({
    activeConversationId: ref(1),
    messages: ref([
      { role: 'user', jobId: 'job', messageId: 1, content: 'question' },
      summary(2, 3), reply, summary(5, 5),
    ]),
    transformServerMessages: records => records,
  })
  const group = timeline.groupedMessages.value[1]
  assert.deepEqual(timeline.groupedMessages.value.map(item => item.type), ['msg', 'process', 'msg'])
  assert.equal(timeline.groupedMessages.value[2].msg.content, 'final answer')
  await timeline.loadProcessDetail(group)
  const loaded = timeline.groupedMessages.value
  assert.equal(loaded[1].key, group.key)
  assert.deepEqual(loaded[1].steps.map(step => step.id), [2, 3, 5])
  assert.equal(loaded[2].msg.content, 'final answer')
  await timeline.loadProcessDetail(loaded[1])
  assert.equal(requests, 1)
})


test('first expansion fetches process once; socket updates and live steps keep the expanded details current', async t => {
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] })
  let timeline
  const subscriptions = []
  const conversationId = ref(1)
  const summary = { role: 'tool', kind: 'process-summary', jobId: 'job', detailOmitted: true,
    status: 'running', processRange: { start_id: 2, end_id: 3 } }
  const messages = ref([summary, { role: 'assistant', jobId: 'job', messageId: 4, content: 'answer' }])
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const mounted = createApp({ setup() {
    const realtime = provideAssistantRealtime(ref(false))
    t.mock.method(realtime, 'subscribe', (topic, params, receive, error) => {
      const subscription = { topic, params, receive, error, stopped: false, refreshes: 0,
        stop() { this.stopped = true }, refresh() { this.refreshes++ } }
      subscriptions.push(subscription)
      return subscription
    })
    timeline = useMessageTimeline({ messages, activeConversationId: conversationId,
      transformServerMessages: records => records.map(record => ({ ...record, jobId: 'job' })) })
    return () => null
  } })
  mounted.mount({})
  t.after(() => mounted.unmount())
  let calls = 0
  t.mock.method(globalThis, 'fetch', async () => {
    calls++
    return { ok: true, json: async () => ({ messages: [
      { id: 'tool', messageId: 2, role: 'tool', kind: 'tool-run', status: 'running', processing_status: 'running' },
    ] }) }
  })
  assert.equal(subscriptions.length, 0)
  assert.equal(calls, 0)
  const original = timeline.groupedMessages.value[0]
  await timeline.loadProcessDetail(original)
  assert.equal(calls, 1)
  assert.equal(subscriptions.length, 1)
  assert.equal(subscriptions[0].topic, 'process')
  assert.deepEqual(subscriptions[0].params, { conversation_id: 1, start_id: 2, end_id: 3, job_id: 'job' })
  subscriptions[0].receive({ messages: [
    { id: 'tool', messageId: 2, role: 'tool', kind: 'tool-run', status: 'completed', output: 'socket result', processing_status: 'completed' },
    { id: 4, role: 'assistant', content: 'answer' },
  ] })
  let loaded = timeline.groupedMessages.value[0]
  assert.equal(loaded.key, original.key)
  assert.equal(loaded.running, false)
  assert.equal(loaded.steps.length, 1)
  assert.equal(loaded.steps[0].output, 'socket result')
  messages.value.splice(1, 0,
    { role: 'tool', kind: 'tool-run', id: 'tool', jobId: 'job', status: 'completed', output: 'live result' },
    { role: 'tool', kind: 'tool-run', id: 'next', jobId: 'job', status: 'running', output: null })
  loaded = timeline.groupedMessages.value[0]
  assert.deepEqual(loaded.steps.map(step => step.id), ['tool', 'next'])
  assert.equal(loaded.steps[0].output, 'live result')
  assert.equal(loaded.running, true)
  await timeline.loadProcessDetail(loaded)
  t.mock.timers.tick(300000)
  assert.equal(calls, 1)
  assert.equal(subscriptions.length, 1)
  messages.value[0].processRange.end_id = 5
  await nextTick()
  loaded = timeline.groupedMessages.value[0]
  assert.equal(loaded.detailKey, original.detailKey)
  assert.equal(loaded.needsDetail, false)
  assert.equal(subscriptions.length, 2)
  assert.equal(subscriptions[0].stopped, true)
  assert.equal(subscriptions[1].params.end_id, 5)
  await timeline.loadProcessDetail(loaded)
  assert.equal(calls, 1)
  subscriptions[1].error('socket unavailable')
  assert.equal(timeline.processDetailErrors.get(loaded.detailKey), 'socket unavailable')
  await timeline.loadProcessDetail(loaded)
  assert.equal(subscriptions[1].refreshes, 1)
  assert.equal(subscriptions.length, 2)
  assert.equal(calls, 1)
  conversationId.value = 2
  messages.value = [{ role: 'assistant', content: 'new conversation' }]
  assert.equal(subscriptions[1].stopped, true)
  subscriptions[0].receive({ messages: [{ role: 'tool', content: 'late old push' }] })
  assert.equal(timeline.groupedMessages.value[0].msg.content, 'new conversation')
})
