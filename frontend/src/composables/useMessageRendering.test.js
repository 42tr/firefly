import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createRenderer, nextTick, ref } from 'vue'
import { marked } from 'marked'
import { createServer } from 'vite'

// Count real Markdown parses while mounting reactive render effects without a DOM.
// Sanitization/code-block DOM integration is unchanged by the cache.
const server = await createServer({ configFile: 'frontend/vite.config.ts',
  server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom',
  ssr: { noExternal: ['dompurify'] },
  plugins: [{ name: 'test-sanitizer', enforce: 'pre',
    resolveId: id => id === 'dompurify' ? '\0test-sanitizer' : null,
    load: id => id === '\0test-sanitizer' ? 'export default { addHook() {}, sanitize(value) { return value } }' : null,
  }],
})
after(() => server.close())
const { useMessageRendering } = await server.ssrLoadModule('/src/composables/useMessageRendering.js')

function mount(t, render) {
  const activeConversation = ref({ cwd: 'project' })
  const isStreaming = ref(false)
  let api, html
  const renderer = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = renderer.createApp({ setup() {
    api = useMessageRendering({ activeConversation, isStreaming, imagePreview: ref(null) })
    return () => { html = render(api); return null }
  } })
  app.mount({})
  let mounted = true
  const unmount = () => { if (mounted) { app.unmount(); mounted = false } }
  t.after(unmount)
  return { api, activeConversation, isStreaming, html: () => html, unmount }
}

test('unchanged conversation history reuses HTML through unrelated updates', async t => {
  const parse = t.mock.method(marked, 'parse')
  const revision = ref(0)
  const messages = ref(Array.from({ length: 100 }, (_, index) => ({ content: `**历史 ${index}**` })))
  const view = mount(t, api => {
    void revision.value
    return messages.value.map(message => api.renderMessageHtml(message))
  })
  assert.equal(parse.mock.callCount(), 100)
  for (let i = 0; i < 20; i++) { revision.value++; await nextTick() }
  assert.equal(parse.mock.callCount(), 100)
  messages.value[50].content = '**修改后的内容**'
  await nextTick()
  assert.equal(parse.mock.callCount(), 101)
  assert.match(view.html()[50], /<strong>修改后的内容<\/strong>/)
})

test('long conversation streams bound parse frequency and render a trailing delta without another event', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 1000 })
  const parse = t.mock.method(marked, 'parse')
  const messages = ref([
    ...Array.from({ length: 100 }, (_, index) => ({ content: `历史 ${index}` })),
    { content: '# 长回复\n\n' + '已有内容。'.repeat(2000) },
  ])
  const view = mount(t, api => messages.value.map(message => api.renderMessageHtml(message)))
  view.isStreaming.value = true
  await nextTick()
  const initial = parse.mock.callCount()
  assert.equal(initial, 101)
  for (let i = 0; i < 80; i++) {
    t.mock.timers.tick(5)
    messages.value.at(-1).content += ` delta${i}`
    await nextTick()
  }
  assert.ok(parse.mock.callCount() - initial <= 11, '80 deltas over 400ms should be batched rather than parsed per delta')
  t.mock.timers.tick(40)
  await nextTick()
  assert.match(view.html().at(-1), /delta79/)
  assert.equal(parse.mock.callCount() - initial, 10)
  // Every other turn stays cached even though the stream invalidates the feed render effect.
  assert.equal(view.html().length, 101)
})

test('ending a stream flushes its final delta immediately', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 1000 })
  const message = ref({ content: 'first' })
  const view = mount(t, api => api.renderMessageHtml(message.value))
  view.isStreaming.value = true
  await nextTick()
  message.value.content += ' final delta'
  await nextTick()
  assert.doesNotMatch(view.html(), /final delta/)
  view.isStreaming.value = false
  await nextTick()
  assert.match(view.html(), /final delta/)
})

test('expensive Markdown parsing increases the budget and still flushes the trailing content', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 1000 })
  const originalParse = marked.parse
  const parse = t.mock.method(marked, 'parse', (...args) => {
    const html = originalParse(...args)
    t.mock.timers.tick(20)
    return html
  })
  const message = ref({ content: 'first' })
  const view = mount(t, api => api.renderMessageHtml(message.value))
  view.isStreaming.value = true
  message.value.content += ' trailing'
  await nextTick()
  t.mock.timers.tick(40)
  await nextTick()
  assert.equal(parse.mock.callCount(), 1)
  t.mock.timers.tick(40)
  await nextTick()
  assert.equal(parse.mock.callCount(), 2)
  assert.match(view.html(), /trailing/)
})

test('switching the working directory invalidates cached links', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 1000 })
  const parse = t.mock.method(marked, 'parse')
  const message = ref({ content: '![报告](/tmp/report.png)' })
  const view = mount(t, api => api.renderMessageHtml(message.value))
  view.isStreaming.value = true
  view.activeConversation.value = { agent: 'firefly', cwd: 'project-a' }
  await nextTick()
  assert.match(view.html(), /path=%2Ftmp%2Freport.png/)
  const calls = parse.mock.callCount()
  view.activeConversation.value = { agent: 'firefly', cwd: 'project-b' }
  await nextTick()
  assert.ok(parse.mock.callCount() > calls, 'changing cwd must invalidate the cached HTML')
})

test('unmounting cancels deferred Markdown work', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 1000 })
  const clear = t.mock.method(globalThis, 'clearTimeout')
  const parse = t.mock.method(marked, 'parse')
  const message = ref({ content: 'first' })
  const view = mount(t, api => api.renderMessageHtml(message.value))
  view.isStreaming.value = true
  await nextTick()
  message.value.content = 'pending'
  await nextTick()
  view.unmount()
  assert.equal(clear.mock.callCount(), 1)
  t.mock.timers.tick(120)
  await nextTick()
  assert.equal(parse.mock.callCount(), 1)
})
