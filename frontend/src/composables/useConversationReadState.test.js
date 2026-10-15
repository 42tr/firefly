import assert from 'node:assert/strict'
import test from 'node:test'
import { createRenderer, nextTick, ref } from 'vue'
import { useConversationReadState } from './useConversationReadState.js'

const conversation = (id, status = 'completed', latest = 2, read = 0) => ({
  id, status, last_message_id: latest, read_message_id: read,
})
function mount(t, { rows = [conversation(1)], hidden = false, saveRead = async (_, id) => id } = {}) {
  const listeners = new Map()
  const document = { hidden, addEventListener: (name, fn) => listeners.set(name, fn), removeEventListener: name => listeners.delete(name) }
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'document')
  Object.defineProperty(globalThis, 'document', { configurable: true, value: document })
  const conversations = ref(rows), activeConversationId = ref(null), isReading = ref(false), readMessageId = ref(0)
  const requests = []
  let state
  const renderer = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = renderer.createApp({ setup() {
    state = useConversationReadState({ conversations, activeConversationId, isReading, readMessageId,
      saveRead: (id, position) => { requests.push([id, position]); return saveRead(id, position) },
    })
    return () => null
  } })
  app.mount({})
  t.after(() => {
    app.unmount()
    if (previous) Object.defineProperty(globalThis, 'document', previous)
    else delete globalThis.document
  })
  return { ...state, conversations, activeConversationId, isReading, readMessageId, requests, document, listeners }
}

test('server read position clears unread on another device and survives reloads', async t => {
  const state = mount(t)
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
  state.conversations.value = [conversation(1, 'completed', 2, 2)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  assert.deepEqual(state.requests, [])
  // A fresh device also uses server state, without a local history baseline.
  state.conversations.value = [conversation(2, 'completed', 4, 4)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  state.conversations.value = [conversation(2, 'completed', 6, 4)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
})

test('reading acknowledges visible messages, while hidden tabs and unloaded replies stay unread', async t => {
  const state = mount(t, { hidden: true })
  state.activeConversationId.value = 1
  state.isReading.value = true
  state.readMessageId.value = 2
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
  assert.deepEqual(state.requests, [])
  state.document.hidden = false
  await state.listeners.get('visibilitychange')()
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  assert.deepEqual(state.requests, [[1, 2]])
  state.conversations.value = [conversation(1, 'completed', 4, 2)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
  state.readMessageId.value = 4
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  assert.deepEqual(state.requests, [[1, 2], [1, 4]])
})

test('only completed replies read at the bottom are acknowledged', async t => {
  const state = mount(t, { rows: [conversation(1, 'running')] })
  state.activeConversationId.value = 1
  state.readMessageId.value = 2
  state.isReading.value = true
  await nextTick()
  assert.deepEqual(state.requests, [])
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  state.isReading.value = false
  state.conversations.value = [conversation(1)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
  state.isReading.value = true
  await nextTick()
  assert.deepEqual(state.requests, [[1, 2]])
})

test('pagination, renames and pinning preserve the server read position', async t => {
  const state = mount(t)
  state.conversations.value = [conversation(2)]
  await nextTick()
  state.conversations.value = [{ ...conversation(1), title: '重命名', is_pinned: true, updated_at: 'later' }]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
  state.conversations.value = [conversation(1, 'running', 4)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  state.conversations.value = [conversation(1, 'completed', 6)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
})

test('a delayed acknowledgement and stale snapshot cannot hide a newer reply', async t => {
  let finish
  const state = mount(t, { saveRead: () => new Promise(resolve => { finish = resolve }) })
  state.activeConversationId.value = 1
  state.isReading.value = true
  state.readMessageId.value = 2
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  // A refresh with the same old read cursor does not undo the pending read.
  state.conversations.value = [conversation(1)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  assert.deepEqual(state.requests, [[1, 2]])
  state.conversations.value = [conversation(1, 'completed', 4)]
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
  finish(2)
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
  assert.deepEqual(state.requests, [[1, 2]])
})

test('a newer visible reply is acknowledged after the pending request finishes', async t => {
  let finish
  const state = mount(t, { saveRead: (_, id) => id === 2 ? new Promise(resolve => { finish = resolve }) : Promise.resolve(id) })
  state.activeConversationId.value = 1
  state.isReading.value = true
  state.readMessageId.value = 2
  await nextTick()
  state.conversations.value = [conversation(1, 'completed', 4)]
  state.readMessageId.value = 4
  await nextTick()
  assert.deepEqual(state.requests, [[1, 2]])
  finish(2)
  await nextTick()
  assert.deepEqual(state.requests, [[1, 2], [1, 4]])
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
})

test('failed writes restore unread and retry when the device becomes visible', async t => {
  let fail = true
  const state = mount(t, { saveRead: async (_, id) => { if (fail) throw new Error('offline'); return id } })
  state.activeConversationId.value = 1
  state.isReading.value = true
  state.readMessageId.value = 2
  await nextTick()
  assert.equal(state.hasUnread(state.conversations.value[0]), true)
  fail = false
  await state.listeners.get('visibilitychange')()
  assert.equal(state.hasUnread(state.conversations.value[0]), false)
  assert.deepEqual(state.requests, [[1, 2], [1, 2]])
})
