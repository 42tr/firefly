import assert from 'node:assert/strict'
import test from 'node:test'
import { createRenderer, ref } from 'vue'
import { createServer } from 'vite'

const server = await createServer({ configFile: 'frontend/vite.config.ts', server: { middlewareMode: true, hmr: false },
  optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom', ssr: { noExternal: ['dompurify'] },
  plugins: [{ name: 'test-sanitizer', enforce: 'pre',
    resolveId: id => id === 'dompurify' ? '\0test-sanitizer' : null,
    load: id => id === '\0test-sanitizer' ? 'export default { addHook() {}, sanitize(value) { return value } }' : null,
  }],
})
test.after(() => server.close())
const { useMessages } = await server.ssrLoadModule('/src/composables/useMessages.js')
const { useMessageTimeline } = await server.ssrLoadModule('/src/composables/useMessageTimeline.js')
const { getProcessDurationMs } = await server.ssrLoadModule('/src/composables/processDuration.js')

test('reloaded compact history uses persisted job duration even when all process records were saved at completion', t => {
  let timeline
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = createApp({ setup() {
    const model = useMessages()
    const timing = { generation_job_id: 'job', processing_status: 'completed',
      processing_started_at: '2026-10-12T01:00:00Z', processing_completed_at: '2026-10-12T01:05:00Z' }
    model.messages.value = model.transformServerMessages([
      { id: 1, role: 'user', content: 'question', created_at: '2026-10-12T00:00:00Z', ...timing },
      { id: 2, role: 'process', started_at: '2026-10-12T01:04:00Z', completed_at: '2026-10-12T01:04:00Z',
        start_id: 2, end_id: 3, status: 'completed', ...timing },
      { id: 4, role: 'assistant', content: 'answer', created_at: '2026-10-12T01:04:00Z', ...timing },
    ])
    timeline = useMessageTimeline({ messages: model.messages, activeConversationId: ref(1) })
    return () => null
  } })
  app.mount({})
  t.after(() => app.unmount())
  const process = timeline.groupedMessages.value.find(item => item.type === 'process')
  assert.equal(process.running, false)
  assert.equal(getProcessDurationMs(process.timingSteps, process.running), 300000)
})

test('history restores three generated images outside folded process details', t => {
  let messages, timeline
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = createApp({ setup() {
    const model = useMessages()
    messages = model.messages
    const record = (id, role, content, extra = {}) => ({ id, role, content, generation_job_id: 'job', ...extra })
    messages.value = model.transformServerMessages([
      record(1, 'user', 'draw three variants'),
      record(2, 'process', '', { start_id: 2, end_id: 10 }),
      ...[1, 2, 3].flatMap(index => [
        record(10 + index * 2, 'assistant', '', { tool_calls: [{ id: `drawing-${index}`, name: 'image_generation', arguments: {} }] }),
        record(11 + index * 2, 'tool', JSON.stringify({ type: 'image', image_url: `/api/assistant/attachments/by-file/${index}`, failure: null }),
          { tool_call_id: `drawing-${index}`, metadata: { tool_name: 'image_generation' } }),
      ]),
      record(30, 'tool', 'cleanup done', { tool_call_id: 'cleanup', metadata: { tool_name: 'command' } }),
      record(31, 'assistant', 'choose 1, 2 or 3'),
    ])
    timeline = useMessageTimeline({ messages, activeConversationId: ref(1), transformServerMessages: model.transformServerMessages })
    return () => null
  } })
  app.mount({})
  t.after(() => app.unmount())
  const images = timeline.groupedMessages.value.filter(item => item.type === 'msg' && item.msg.imageOutput)
  assert.equal(images.length, 3)
  assert.deepEqual(images.map(item => item.msg.messageId), [13, 15, 17])
  assert.equal(messages.value.some(message => message.name === 'image_generation'), false)
  assert.equal(timeline.groupedMessages.value.at(-1).msg.content, 'choose 1, 2 or 3')
})
