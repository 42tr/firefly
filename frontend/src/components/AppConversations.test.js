import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createRenderer, nextTick, ssrContextKey } from 'vue'
import { createServer } from 'vite'

// Mount the app's real conversation state without its visual tree or a model.
const storage = () => {
  const values = new Map()
  return { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) }
}
const eventTarget = () => ({ addEventListener() {}, removeEventListener() {} })
const location = { href: 'http://localhost/', pathname: '/' }
const browserWindow = {
  ...eventTarget(), location, sessionStorage: storage(),
  history: { replaceState: (_, __, url) => { location.href = String(url) } },
  dispatchEvent() {},
}
const server = await createServer({ configFile: 'frontend/vite.config.ts',
  server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom',
  ssr: { noExternal: ['dompurify'] },
  plugins: [{ name: 'test-sanitizer', enforce: 'pre',
    resolveId: id => id === 'dompurify' ? '\0test-sanitizer' : null,
    load: id => id === '\0test-sanitizer' ? 'export default { addHook() {}, sanitize(value) { return value } }' : null,
  }],
})
after(() => server.close())
// App initializes its URL when its module loads.
const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
Object.defineProperty(globalThis, 'window', { configurable: true, value: browserWindow })
const { default: App } = await server.ssrLoadModule('/src/App.vue')
if (previousWindow) Object.defineProperty(globalThis, 'window', previousWindow)
else delete globalThis.window

async function mount(t, fetcher) {
  const previous = new Map(['window', 'document', 'localStorage'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]))
  Object.defineProperty(globalThis, 'window', { configurable: true, value: browserWindow })
  Object.defineProperty(globalThis, 'document', { configurable: true, value: { ...eventTarget(), body: { classList: { toggle() {}, remove() {} } } } })
  Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: storage() })
  browserWindow.sessionStorage = storage()
  location.href = 'http://localhost/'
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    if (String(url).endsWith('/ping')) return { ok: false, status: 401 }
    return fetcher(String(url), options)
  })
  let state
  const renderer = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = renderer.createApp({ ...App,
    setup(props, context) { state = App.setup(props, context); return state }, render: () => null,
  })
  app.provide(ssrContextKey, { modules: new Set() })
  app.mount({})
  t.after(() => {
    app.unmount()
    for (const [key, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else delete globalThis[key]
    }
  })
  await nextTick()
  return state
}
const response = data => ({ ok: true, json: async () => data })
const conversation = (id, agent = 'firefly') => ({ id, agent, title: `对话 ${id}`, message_count: 2,
  last_message_id: 10, read_message_id: 0 })

test('completed background conversations become unread and clear only after their messages load', async t => {
  let failMessages = true
  const state = await mount(t, url => {
    if (url.includes('/messages') && failMessages) return { ok: false, text: async () => '加载失败' }
    if (url.includes('/messages')) return response({ messages: [{ id: 10, conversation_id: 2, role: 'assistant', content: '完成的回复' }] })
    return response({ active: false })
  })
  state.conversations.value = [{ ...conversation(2), status: 'running' }]
  state.conversationIds.value = [2]
  await nextTick()
  state.conversations.value = [{ ...conversation(2), status: 'completed', message_count: 3 }]
  await nextTick()
  assert.equal(state.visibleConversations.value[0].hasUnread, true)
  await state.selectConversation(2)
  await nextTick()
  assert.equal(state.visibleConversations.value[0].hasUnread, true)
  failMessages = false
  await state.selectConversation(2, { force: true })
  await nextTick()
  assert.equal(state.visibleConversations.value[0].hasUnread, false)
})

test('completion while a module is open or while reading history remains unread until the newest messages are visible', async t => {
  const state = await mount(t, () => response({}))
  state.conversations.value = [{ ...conversation(2), status: 'running' }]
  state.conversationIds.value = [2]
  state.activeConversationId.value = 2
  state.messages.value = [{ role: 'assistant', content: '回复', messageId: 10 }]
  state.modulePageOpen.value = true
  await nextTick()
  state.conversations.value = [{ ...conversation(2), status: 'completed' }]
  await nextTick()
  assert.equal(state.visibleConversations.value[0].hasUnread, true)
  state.isPinnedToBottom.value = false
  state.modulePageOpen.value = false
  await nextTick()
  assert.equal(state.visibleConversations.value[0].hasUnread, true)
  state.isPinnedToBottom.value = true
  await nextTick()
  assert.equal(state.visibleConversations.value[0].hasUnread, false)
})

test('reading on another device clears this sidebar through the server snapshot', async t => {
  const state = await mount(t, () => response({}))
  state.conversations.value = [{ ...conversation(2), status: 'completed' }]
  state.conversationIds.value = [2]
  await nextTick()
  assert.equal(state.visibleConversations.value[0].hasUnread, true)
  await state.applyConversations({ conversations: [{ ...conversation(2), status: 'completed', read_message_id: 10 }] })
  await nextTick()
  assert.equal(state.visibleConversations.value[0].hasUnread, false)
})

test('default Firefly uses a pending conversation and keeps its draft when opening a module', async t => {
  const requests = []
  const state = await mount(t, url => { requests.push(url); return response({ conversations: [] }) })
  await state.openDefaultConversation()
  assert.equal(state.pendingConversation.value.agent, 'firefly')
  assert.equal(state.activeConversationId.value, null)
  assert.equal(state.inputDisabled.value, false)
  assert.deepEqual(requests, [])
  state.workspaceInitialized = true
  state.input.value = '未发送的内容'
  state.modulePageOpen.value = true
  state.modulePageOpen.value = false
  assert.equal(state.input.value, '未发送的内容')
})

test('Firefly selection loads its own history, restores source information and uses the shared sidebar', async t => {
  const requests = []
  const source = { message_id: 10, conversation_id: 1, preview: '已有回复' }
  const state = await mount(t, url => {
    requests.push(url)
    if (url.includes('/messages')) return response({ messages: [{ id: 20, conversation_id: 2, role: 'user', content: '继续', attachments: [] }], source_reply: source })
    if (url.endsWith('/active-generation')) return response({ active: false })
    return response({})
  })
  state.conversations.value = [conversation(2), conversation(3, 'codex')]
  state.conversationIds.value = [2, 3]
  await state.selectConversation(2)
  assert.equal(state.activeConversationId.value, 2)
  assert.equal(state.messages.value[0].content, '继续')
  assert.deepEqual(state.conversationSource.value, source)
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2, 3])
  assert.match(location.href, /conversation=2/)
  assert.ok(requests.some(url => url.includes('/conversations/2/messages')))
  assert.ok(requests.every(url => !url.includes('/firefly/messages')))
})

test('new Firefly sends successive turns into the same conversation through the public message endpoint', async t => {
  const submissions = []
  let creations = 0
  const state = await mount(t, (url, options) => {
    if (url === '/api/assistant/conversations' && options?.method === 'POST') {
      creations++
      assert.equal(JSON.parse(options.body).agent, 'firefly')
      return response(conversation(4))
    }
    if (url.endsWith('/conversations/4/messages') && options?.method === 'POST') {
      submissions.push(options.body.get('message'))
      return response({ status: 'completed', message: { id: 30 + submissions.length, attachments: [] } })
    }
    return response({ conversations: [conversation(4)], selected_conversation: conversation(4) })
  })
  await state.openDefaultConversation()
  state.input.value = '第一问'
  await state.sendMessage()
  state.input.value = '接着问'
  await state.sendMessage()
  assert.equal(creations, 1)
  assert.deepEqual(submissions, ['第一问', '接着问'])
  assert.equal(state.activeConversationId.value, 4)
})

test('navigating during lazy creation does not replace the selected conversation or submit into it', async t => {
  let finishCreation
  const submissions = []
  const state = await mount(t, (url, options) => {
    if (options?.method === 'POST' && url === '/api/assistant/conversations') return new Promise(resolve => { finishCreation = resolve })
    if (options?.method === 'POST') submissions.push(url)
    if (url.includes('/messages')) return response({ messages: [] })
    return response({ active: false })
  })
  await state.openDefaultConversation()
  state.input.value = '新对话的问题'
  const sending = state.sendMessage()
  state.conversations.value = [conversation(9, 'claude')]
  await state.selectConversation(9)
  finishCreation(response(conversation(4)))
  await sending
  assert.equal(state.activeConversationId.value, 9)
  assert.equal(state.input.value, '')
  assert.deepEqual(submissions, [])
})

test('opening a Firefly URL restores the requested conversation and allows new messages', async t => {
  const selected = conversation(7)
  const state = await mount(t, url => {
    if (url.includes('/messages')) return response({ messages: [{ id: 70, role: 'assistant', conversation_id: 7, content: '保存的历史回复' }] })
    return response({ conversations: [selected], selected_conversation: selected })
  })
  location.href = 'http://localhost/?conversation=7'
  await state.ensureConversations()
  assert.equal(state.activeConversationId.value, 7)
  assert.equal(state.inputDisabled.value, false)
  assert.equal(state.messages.value[0].content, '保存的历史回复')
})

test('a source link resolves an old parent outside the currently loaded page', async t => {
  const parent = conversation(8)
  const state = await mount(t, url => {
    if (url.includes('/messages')) return response({ messages: [{ id: 80, conversation_id: 8, role: 'assistant', content: '来源回复' }] })
    return response({ conversations: [conversation(9)], selected_conversation: parent })
  })
  state.conversations.value = [conversation(9)]
  state.activeConversationId.value = 9
  state.conversationSource.value = { conversation_id: 8, preview: '来源回复' }
  await state.openConversationSource()
  assert.equal(state.activeConversationId.value, 8)
  assert.equal(state.inputDisabled.value, false)
  assert.equal(state.messages.value[0].content, '来源回复')
})

test('new conversation opens directly as an empty draft without creating a server conversation', async t => {
  const requests = []
  const state = await mount(t, url => { requests.push(url); return response({ messages: [] }) })
  state.conversations.value = [conversation(12, 'codex')]
  state.activeConversationId.value = 12
  await nextTick()
  await state.openNewConversationDialog()
  assert.equal(state.activeConversationId.value, null)
  assert.equal(state.pendingConversation.value.agent, 'codex')
  assert.deepEqual(state.messages.value, [])
  assert.equal(new URL(location.href).searchParams.has('conversation'), false)
  assert.deepEqual(requests, [])
})

test('changing the draft agent and directory preserves text and attachments and sends selected settings', async t => {
  const creations = []
  const state = await mount(t, (url, options) => {
    if (url === '/api/assistant/conversations' && options?.method === 'POST') {
      creations.push(JSON.parse(options.body))
      return response(conversation(15, 'codex'))
    }
    return response({ conversations: [] })
  })
  await state.openDefaultConversation()
  state.input.value = '帮我优化移动端'
  const attachment = { name: 'design.png' }
  state.chatAttachments.value = [attachment]
  state.updatePendingConversation({ agent: 'codex', cwd: 'projects/firefly', provider: 'relay', model: 'model-b', reasoning_level: 'high', workspace_mode: 'parallel' })
  assert.equal(state.input.value, '帮我优化移动端')
  assert.deepEqual(state.chatAttachments.value[0], attachment)
  assert.equal(state.lastUsedAgent.value, 'codex')
  await state.ensurePendingConversation()
  assert.equal(creations.length, 1)
  assert.equal(creations[0].agent, 'codex')
  assert.equal(creations[0].cwd, 'projects/firefly')
  assert.equal(creations[0].workspace_mode, 'parallel')
  assert.equal(creations[0].model, 'model-b')
  assert.equal(creations[0].reasoning_level, 'high')
})

test('draft creation controls cannot change a conversation while its first send is creating it', async t => {
  let finish
  const state = await mount(t, () => new Promise(resolve => { finish = resolve }))
  await state.openDefaultConversation()
  const creating = state.ensurePendingConversation()
  state.updatePendingConversation({ agent: 'claude', cwd: 'another-directory' })
  assert.equal(state.pendingConversation.value.agent, 'firefly')
  finish(response(conversation(16)))
  await creating
  assert.equal(state.activeConversationId.value, 16)
})


test('archived history is hidden from the normal sidebar and becomes editable after restoring', async t => {
  let selected = { ...conversation(7), is_archived: true }
  const patches = []
  const state = await mount(t, (url, options) => {
    if (options?.method === 'PATCH') {
      patches.push(JSON.parse(options.body))
      selected = { ...selected, ...patches.at(-1) }
      return response({ ok: true })
    }
    if (url.includes('/messages')) return response({ messages: [{ id: 70, role: 'assistant', content: '保存的历史回复' }] })
    return response({ conversations: selected.is_archived ? [] : [selected], selected_conversation: selected })
  })
  state.conversations.value = [selected]
  state.conversationIds.value = [7]
  await state.selectConversation(7)
  assert.equal(state.inputDisabled.value, true)
  assert.equal(state.visibleConversations.value.length, 0)
  await state.showMoreConversations()
  // Expanding history reveals archives without replacing the active list.
  assert.equal(state.visibleConversations.value[0].id, 7)
  await state.handleToggleConversationArchive(selected)
  assert.deepEqual(patches, [{ is_archived: false }])
  assert.equal(state.inputDisabled.value, false)
  assert.equal(state.showingArchivedConversations.value, true)
  assert.equal(state.visibleConversations.value[0].id, 7)
  assert.equal(state.messages.value[0].content, '保存的历史回复')
})

test('load-more appends archived history after unarchived conversations, including pinned archives', async t => {
  const active = conversation(2)
  const archived = { ...conversation(7), is_archived: true, is_pinned: true }
  const state = await mount(t, url => {
    const archiveQuery = new URL(url, 'http://localhost').searchParams.get('archived') === 'true'
    return response({ conversations: archiveQuery ? [archived] : [active] })
  })
  await state.fetchConversations()
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2])
  assert.equal(state.hasMoreConversations.value, true)
  await state.showMoreConversations()
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2, 7])
  assert.equal(state.hasMoreConversations.value, false)
  state.conversationSearch.value = '工作'
  await nextTick()
  await new Promise(setImmediate)
  assert.equal(state.showingArchivedConversations.value, false)
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2])
})

test('archives can collapse and reopen without paging the active list or losing loaded history', async t => {
  const queries = []
  const state = await mount(t, url => {
    const query = new URL(url, 'http://localhost').searchParams
    queries.push(query)
    if (query.get('archived') === 'true') return response({
      conversations: [{ ...conversation(7), is_archived: true }], next_cursor: { before_id: 7 },
    })
    return response({ conversations: [conversation(2)], next_cursor: { before_id: 2 } })
  })
  await state.fetchConversations()
  await state.toggleArchivedConversations()
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2, 7])
  assert.deepEqual(queries.map(query => query.get('archived')), ['false', 'true'])
  await state.toggleArchivedConversations()
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2])
  await state.toggleArchivedConversations()
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2, 7])
  assert.equal(queries.length, 2)
  assert.equal(state.hasMoreConversations.value, true)
})

test('collapsing archives during loading keeps late results hidden and available to reopen', async t => {
  let finishArchive
  const state = await mount(t, url => {
    if (new URL(url, 'http://localhost').searchParams.get('archived') === 'true') {
      return new Promise(resolve => { finishArchive = resolve })
    }
    return response({ conversations: [conversation(2)] })
  })
  await state.fetchConversations()
  const expansion = state.toggleArchivedConversations()
  assert.equal(state.isLoadingConversations.value, true)
  assert.equal(state.showingArchivedConversations.value, true)
  await state.toggleArchivedConversations()
  assert.equal(state.showingArchivedConversations.value, false)
  finishArchive(response({ conversations: [{ ...conversation(7), is_archived: true }] }))
  await expansion
  assert.equal(state.showingArchivedConversations.value, false)
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2])
  await state.toggleArchivedConversations()
  assert.deepEqual(state.visibleConversations.value.map(item => item.id), [2, 7])
})
