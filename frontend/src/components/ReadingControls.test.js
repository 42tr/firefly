import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createRenderer, h, nextTick, ref, ssrContextKey } from 'vue'
import { createServer } from 'vite'

// Check control behavior with the project's renderer; these are not visual/browser tests.
const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)),
  server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { default: Sidebar } = await server.ssrLoadModule('/src/components/ConversationSidebar.vue')
const { default: RecentChats } = await server.ssrLoadModule('/src/components/RecentChats.vue')
const { default: SearchDialog } = await server.ssrLoadModule('/src/components/ConversationSearchDialog.vue')
const providers = await Promise.all([
  ['useModules', 'provideModules'],
].map(async ([file, name]) => (await server.ssrLoadModule(`/src/composables/${file}.js`))[name]))

function mount(t, Component, getProps, provideStates = false) {
  const listeners = new Map()
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, 'document')
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const target = prefix => ({
    addEventListener(type, handler) {
      const key = prefix + type
      if (!listeners.has(key)) listeners.set(key, new Set())
      listeners.get(key).add(handler)
    },
    removeEventListener(type, handler) { listeners.get(prefix + type)?.delete(handler) },
  })
  Object.defineProperty(globalThis, 'document', { configurable: true, value: target('document:') })
  Object.defineProperty(globalThis, 'window', { configurable: true, value: target('window:') })
  let state
  const Control = { ...Component, setup(props, context) { state = Component.setup(props, context); return state }, render: () => null }
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = createApp({ setup() {
    if (provideStates) providers.forEach(provide => provide(ref(false)))
    return () => h(Control, getProps())
  } })
  app.provide(ssrContextKey, { modules: new Set() })
  app.mount({})
  const unmount = () => app.unmount()
  t.after(() => {
    unmount()
    if (previousDocument) Object.defineProperty(globalThis, 'document', previousDocument)
    else delete globalThis.document
    if (previousWindow) Object.defineProperty(globalThis, 'window', previousWindow)
    else delete globalThis.window
  })
  return { state, listeners, unmount }
}

test('sidebar collapse retains the active workspace and restores focus to a visible control in both directions', async t => {
  const collapsed = ref(false)
  const { state } = mount(t, Sidebar, () => ({
    collapsed: collapsed.value, activeConversationId: 7,
    conversations: [{ id: 7, cwd: '/work/project', agent: 'codex' }],
    formatConversationTitle: () => '当前工作', formatConversationMeta: () => '',
    onToggleCollapse: () => { collapsed.value = !collapsed.value },
  }), true)
  const focused = []
  state.expandButton.value = { focus: () => focused.push('expand') }
  state.panel.value = { querySelector: () => ({ focus: () => focused.push('brand') }) }
  await state.toggleCollapsed()
  assert.equal(collapsed.value, true)
  assert.equal(state.props.activeConversationId, 7)
  await state.toggleCollapsed()
  assert.equal(collapsed.value, false)
  assert.equal(state.props.activeConversationId, 7)
  assert.deepEqual(focused, ['expand', 'brand'])
})

test('conversation search uses a modal, locks scrolling, and restores focus and scrolling after close', async t => {
  const open = ref(false)
  const { state } = mount(t, SearchDialog, () => ({ open: open.value, formatTitle: String }))
  const focused = []
  document.body = { style: { overflow: 'auto' } }
  document.activeElement = { isConnected: true, focus: () => focused.push('trigger') }
  const modal = { open: false, showModal() { this.open = true }, close() { this.open = false } }
  state.dialog.value = modal
  state.input.value = { focus: () => focused.push('input') }
  open.value = true
  await nextTick()
  await nextTick()
  assert.equal(modal.open, true)
  assert.equal(document.body.style.overflow, 'hidden')
  open.value = false
  await nextTick()
  assert.equal(modal.open, false)
  assert.equal(document.body.style.overflow, 'auto')
  assert.deepEqual(focused, ['input', 'trigger'])
})

test('search Enter respects IME composition and does not choose stale results while loading or failed', async t => {
  const loading = ref(false), error = ref(''), selected = [], focused = []
  let closes = 0
  const { state } = mount(t, SearchDialog, () => ({ loading: loading.value, error: error.value,
    conversations: [{ id: 7 }], formatTitle: String, onSelect: id => selected.push(id), onClose: () => closes++ }))
  state.results.value = { querySelector: () => ({ focus: () => focused.push('result') }) }
  const key = (name, extra = {}) => ({ key: name, preventDefault() {}, ...extra })
  state.handleInputKeydown(key('Enter', { isComposing: true }))
  state.handleInputKeydown(key('Enter', { keyCode: 229 }))
  assert.deepEqual(selected, [])
  loading.value = true
  await nextTick()
  state.handleInputKeydown(key('Enter'))
  state.select(7)
  loading.value = false
  error.value = 'failed'
  await nextTick()
  state.handleInputKeydown(key('Enter'))
  assert.deepEqual(selected, [])
  error.value = ''
  await nextTick()
  state.handleInputKeydown(key('ArrowDown'))
  state.handleInputKeydown(key('Enter'))
  assert.deepEqual(focused, ['result'])
  assert.deepEqual(selected, [7])
  state.handleDialogKeydown(key('Escape', { isComposing: true }))
  assert.equal(closes, 0)
  let stopped = false, prevented = false
  state.handleDialogKeydown({ key: 'Escape', stopPropagation: () => { stopped = true }, preventDefault: () => { prevented = true } })
  assert.equal(closes, 1)
  assert.equal(stopped, true)
  assert.equal(prevented, true)
})

test('closing sidebar search clears the query and selects the original conversation through workspace navigation', t => {
  const selected = [], queries = []
  const { state } = mount(t, Sidebar, () => ({ search: '查询',
    formatConversationTitle: String, formatConversationMeta: String,
    'onUpdate:search': value => queries.push(value), onSelectRecent: id => selected.push(id),
  }), true)
  state.openSearch()
  assert.equal(state.searchOpen.value, true)
  state.selectSearchResult(7)
  assert.equal(state.searchOpen.value, false)
  assert.deepEqual(queries, [''])
  assert.deepEqual(selected, [7])
})

test('desktop Ctrl and Command K open search and the shortcut listener is removed on unmount', t => {
  const { state, listeners, unmount } = mount(t, Sidebar, () => ({
    formatConversationTitle: String, formatConversationMeta: String,
  }), true)
  document.querySelector = () => null
  const handlers = listeners.get('document:keydown')
  assert.equal(handlers.has(state.handleSearchShortcut), true)
  for (const modifiers of [{ ctrlKey: true }, { metaKey: true }]) {
    let prevented = false
    for (const handler of handlers) handler({ key: 'k', ...modifiers, preventDefault() { prevented = true } })
    assert.equal(prevented, true)
    assert.equal(state.searchOpen.value, true)
    state.closeSearch()
  }
  unmount()
  assert.equal(handlers.has(state.handleSearchShortcut), false)
})

test('search shortcuts preserve editing, IME composition, modal focus and mobile behavior', async t => {
  const { state } = mount(t, Sidebar, () => ({
    formatConversationTitle: String, formatConversationMeta: String,
  }), true)
  document.querySelector = () => null
  const shortcuts = [
    { ctrlKey: false }, { defaultPrevented: true }, { isComposing: true }, { keyCode: 229 },
    { altKey: true }, { shiftKey: true }, { key: 'j' },
    { target: { closest: () => ({}) } },
  ]
  const key = extra => ({ key: 'k', ctrlKey: true, preventDefault() { assert.fail('typing must keep its key event') }, ...extra })
  for (const extra of shortcuts) {
    state.handleSearchShortcut(key(extra))
    assert.equal(state.searchOpen.value, false)
  }
  document.querySelector = () => ({ open: true })
  state.handleSearchShortcut(key({}))
  assert.equal(state.searchOpen.value, false)
  document.querySelector = () => null
  state.isMobile.value = true
  await nextTick()
  state.handleSearchShortcut(key({}))
  assert.equal(state.searchOpen.value, false)
})

test('recent chats show the latest ten conversations across assistants without changing the source list', t => {
  const conversations = Array.from({ length: 12 }, (_, i) => ({ id: i + 1, agent: 'codex', message_count: 1, updated_at: `2026-10-${String(i + 1).padStart(2, '0')}T00:00:00Z` }))
  const hidden = [
    { id: 20, agent: 'firefly', message_count: 1, updated_at: '2026-10-30T00:00:00Z' },
    { id: 22, agent: 'codex', message_count: 0, updated_at: '2026-10-30T00:00:00Z' },
    { id: 23, agent: 'claude', message_count: 1, is_archived: true, updated_at: '2026-10-31T00:00:00Z' },
  ]
  const source = [...conversations, ...hidden]
  const { state } = mount(t, RecentChats, () => ({ conversations: source, formatTitle: item => String(item.id) }))
  assert.deepEqual(state.recent.value.map(item => item.id), [20, 12, 11, 10, 9, 8, 7, 6, 5, 4])
  assert.deepEqual(source.map(item => item.id), [...conversations, ...hidden].map(item => item.id))
})

test('recent chat navigation opens at the first or last item, restores focus, and selects the original conversation ID', async t => {
  const selected = [], focused = []
  let refreshes = 0
  const { state } = mount(t, RecentChats, () => ({ formatTitle: () => '', onSelect: id => selected.push(id), onRefresh: () => refreshes++ }))
  const buttons = ['first', 'last'].map(name => ({ focus() { focused.push(name); document.activeElement = this } }))
  state.trigger.value = { getBoundingClientRect: () => ({ right: 50, top: 180 }), focus: () => focused.push('trigger') }
  state.menu.value = { offsetHeight: 140, querySelectorAll: () => buttons, focus: () => focused.push('menu') }
  await state.show()
  assert.equal(state.open.value, true)
  assert.equal(refreshes, 1)
  assert.deepEqual(focused, ['first'])
  const event = key => ({ key, preventDefault() {}, stopPropagation() {} })
  state.navigate(event('ArrowDown'))
  assert.equal(document.activeElement, buttons[1])
  state.navigate(event('Home'))
  assert.equal(document.activeElement, buttons[0])
  state.navigate(event('Escape'))
  assert.equal(state.open.value, false)
  assert.equal(focused.at(-1), 'trigger')
  await state.show(true)
  assert.equal(focused.at(-1), 'last')
  state.select({ id: 7 })
  assert.deepEqual(selected, [7])
  assert.equal(state.open.value, false)
})

test('recent chat popup accepts inside and touch focus transitions, closes outside or when hidden, and cleans up listeners', async t => {
  const active = ref(true)
  const { state, listeners, unmount } = mount(t, RecentChats, () => ({ active: active.value, formatTitle: () => '' }))
  const inside = {}, outside = {}
  state.root.value = { contains: node => node === inside }
  state.menu.value = { contains: node => node === inside }
  state.open.value = true
  for (const handler of listeners.get('document:pointerdown')) handler({ target: inside })
  state.focusOut({ relatedTarget: null })
  assert.equal(state.open.value, true)
  for (const handler of listeners.get('document:pointerdown')) handler({ target: outside })
  assert.equal(state.open.value, false)
  state.open.value = true
  active.value = false
  await nextTick()
  assert.equal(state.open.value, false)
  active.value = true
  await nextTick()
  assert.equal(state.open.value, false)
  unmount()
  for (const handlers of listeners.values()) assert.equal(handlers.size, 0)
})

test('recent chat popup stays inside a short viewport using its layout height instead of animated bounds', async t => {
  const { state } = mount(t, RecentChats, () => ({ formatTitle: () => '' }))
  window.innerWidth = 800
  window.innerHeight = 240
  state.open.value = true
  state.trigger.value = { getBoundingClientRect: () => ({ right: 780, top: 180 }) }
  state.menu.value = { offsetHeight: 216, getBoundingClientRect: () => ({ height: 200 }) }
  await state.reposition()
  assert.equal(state.menuStyle.value.top, '12px')
  assert.equal(state.menuStyle.value.left, '468px')
  assert.equal(state.menuStyle.value.maxHeight, '216px')
})

test('retry keeps focus inside recent chats when the error button disappears and ignores a pending refresh', async t => {
  const loading = ref(false), events = []
  const { state } = mount(t, RecentChats, () => ({ loading: loading.value, formatTitle: () => '', onRefresh: () => events.push('refresh') }))
  state.menu.value = { focus: () => events.push('menu') }
  state.open.value = true
  state.refresh()
  assert.deepEqual(events, ['menu', 'refresh'])
  assert.equal(state.open.value, true)
  loading.value = true
  await nextTick()
  state.refresh()
  assert.deepEqual(events, ['menu', 'refresh'])
})
