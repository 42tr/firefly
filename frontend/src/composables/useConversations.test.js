import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createServer } from 'vite'
import { ref } from 'vue'

const server = await createServer({ configFile: 'frontend/vite.config.ts', server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { useConversations } = await server.ssrLoadModule('/src/composables/useConversations.js')
const response = (conversations, next_cursor = null) => ({ ok: true, json: async () => ({ conversations, next_cursor }) })
const cursor = { before_at: '2026-09-27T00:00:00.123456Z', before_id: 20 }

test('conversation creation sends reasoning level for Codex and Claude', async t => {
  const payloads = []
  t.mock.method(globalThis, 'fetch', async (_url, options) => {
    payloads.push(JSON.parse(options.body))
    return { ok: true, json: async () => ({ id: payloads.length }) }
  })
  const app = useConversations()
  await app.createConversation({ agent: 'codex', model: 'model-a', reasoning_level: 'high' })
  assert.equal(payloads[0].reasoning_level, 'high')
  await app.createConversation({ agent: 'codex' })
  assert.ok(!('reasoning_level' in payloads[1]))
  await app.createConversation({ agent: 'claude', reasoning_level: 'high' })
  assert.equal(payloads[2].reasoning_level, 'high')
})

test('conversation polling defaults to unarchived records', async t => {
  const app = useConversations()
  const urls = []
  t.mock.method(globalThis, 'fetch', async url => { urls.push(url); return response([]) })
  await app.fetchConversations()
  await app.fetchConversations({ silent: true })
  assert.deepEqual(urls, ['/api/assistant/conversations?limit=20&search=&archived=false', '/api/assistant/conversations?limit=20&search=&archived=false'])
})

test('Claude creation sends the model and its sidebar metadata displays the selection', async t => {
  let payload
  t.mock.method(globalThis, 'fetch', async (_url, options) => {
    payload = JSON.parse(options.body)
    return { ok: true, json: async () => ({ id: 3, agent: 'claude', claude_model: payload.model }) }
  })
  const app = useConversations()
  const conversation = await app.createConversation({ agent: 'claude', model: 'opus', provider: 'relay', reasoning_level: 'high' })
  assert.equal(payload.model, 'opus')
  assert.equal(payload.provider, 'relay')
  assert.equal(payload.reasoning_level, 'high')
  assert.ok(app.formatConversationMeta(conversation).includes('opus'))
})

test('a pushed title wins over an older HTTP list response', async t => {
  const app = useConversations()
  let finish
  t.mock.method(globalThis, 'fetch', () => new Promise(resolve => { finish = resolve }))
  const old = app.fetchConversations()
  app.applyConversations({ conversations: [{ id: 1, title: '异步生成的新标题' }] })
  finish(response([{ id: 1, title: '新的工作' }]))
  await old
  assert.equal(app.conversations.value[0].title, '异步生成的新标题')
})

const workspace = id => ({ id, agent: 'codex', message_count: 1, title: `工作 ${id}` })

test('pinning persists via the API and reloads all loaded pages in pinned order', async t => {
  const rows = [workspace(3), workspace(2), workspace(1)]
  const patches = []
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    if (options?.method === 'PATCH') {
      const fields = JSON.parse(options.body)
      patches.push(fields)
      rows.find(row => row.id === 1).is_pinned = fields.is_pinned
      return { ok: true }
    }
    const sorted = [...rows].sort((a, b) => Number(Boolean(b.is_pinned)) - Number(Boolean(a.is_pinned)) || b.id - a.id)
    const query = new URL(url, 'http://localhost').searchParams
    const start = query.has('before_id') ? sorted.findIndex(row => row.id === Number(query.get('before_id'))) + 1 : 0
    const page = sorted.slice(start, start + 2)
    return response(page, start + 2 < sorted.length ? {
      ...cursor, before_pinned: Boolean(page.at(-1).is_pinned), before_id: page.at(-1).id,
    } : null)
  })
  const app = useConversations()
  await app.fetchConversations()
  await app.fetchConversations({ more: true })
  await app.setConversationPinned(1, true)
  assert.deepEqual(patches, [{ is_pinned: true }])
  assert.deepEqual(app.conversationIds.value, [1, 3, 2])
  const reloaded = useConversations()
  await reloaded.fetchConversations()
  assert.equal(reloaded.conversations.value[0].id, 1)
  assert.equal(reloaded.conversations.value[0].is_pinned, true)
  await app.setConversationPinned(1, false)
  assert.deepEqual(app.conversationIds.value, [3, 2, 1])
  assert.deepEqual(patches, [{ is_pinned: true }, { is_pinned: false }])
})

test('pin failures preserve the existing state and duplicate clicks cannot race', async t => {
  let finish
  let requests = 0
  t.mock.method(globalThis, 'fetch', () => {
    requests++
    return new Promise(resolve => { finish = resolve })
  })
  const app = useConversations()
  app.conversations.value = [{ ...workspace(1), is_pinned: false }]
  const saving = app.setConversationPinned(1, true)
  assert.deepEqual(app.pinningConversationIds.value, [1])
  assert.equal(await app.setConversationPinned(1, true), false)
  assert.equal(requests, 1)
  finish({ ok: false, text: async () => '无法保存' })
  await assert.rejects(saving, /无法保存/)
  assert.equal(app.conversations.value[0].is_pinned, false)
  assert.deepEqual(app.pinningConversationIds.value, [])
})

test('pagination carries the pinned boundary through to the next request', async t => {
  const urls = []
  t.mock.method(globalThis, 'fetch', async url => {
    urls.push(url)
    return urls.length === 1
      ? response([{ ...workspace(1), is_pinned: true }], { ...cursor, before_pinned: true, before_id: 1 })
      : response([workspace(3)])
  })
  const app = useConversations()
  await app.fetchConversations()
  await app.fetchConversations({ more: true })
  assert.equal(new URL(urls[1], 'http://localhost').searchParams.get('before_pinned'), 'true')
  assert.deepEqual(app.conversationIds.value, [1, 3])
})

test('workspaces paginate past 200 records and keep all loaded pages after a realtime refresh', async t => {
  const app = useConversations()
  const workspaces = Array.from({ length: 225 }, (_, index) => workspace(225 - index))
  const urls = []
  const page = before => {
    const start = before ? workspaces.findIndex(item => item.id === Number(before)) + 1 : 0
    const rows = workspaces.slice(start, start + 20)
    return { conversations: rows, next_cursor: start + 20 < workspaces.length
      ? { ...cursor, before_id: rows.at(-1).id } : null }
  }
  t.mock.method(globalThis, 'fetch', async url => {
    const query = new URL(url, 'http://localhost').searchParams
    urls.push(query)
    return { ok: true, json: async () => page(query.get('before_id')) }
  })
  await app.fetchConversations()
  assert.equal(app.conversationIds.value.length, 20)
  assert.equal(urls.length, 1)
  while (app.conversationNextCursor.value) await app.fetchConversations({ more: true })
  assert.equal(app.conversationIds.value.length, 225)
  assert.equal(app.conversationIds.value.at(-1), 1)
  assert.equal(urls.length, 12)
  assert.equal(await app.fetchConversations({ more: true }), false)
  // Remote deletion/renaming must update later pages without truncating to the head page.
  workspaces.splice(workspaces.findIndex(item => item.id === 100), 1)
  workspaces.at(-1).title = '最早的工作区已重命名'
  await app.applyConversations(page())
  assert.equal(app.conversationIds.value.length, 224)
  assert.equal(app.conversationIds.value.at(-1), 1)
  assert.equal(app.conversations.value.find(item => item.id === 1).title, '最早的工作区已重命名')
  assert.equal(app.conversations.value.some(item => item.id === 100), false)
})

test('workspace search requests the server, resets pagination, and rejects a stale page', async t => {
  const app = useConversations()
  let finish, oldSignal
  const queries = []
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    const query = new URL(url, 'http://localhost').searchParams
    queries.push(query)
    if (queries.length === 1) return response([workspace(21), workspace(20)], cursor)
    if (queries.length === 2) {
      oldSignal = options.signal
      return new Promise(resolve => { finish = resolve })
    }
    return response([workspace(1)])
  })
  await app.fetchConversations()
  const stale = app.fetchConversations({ more: true })
  await app.fetchConversations({ search: '旧项目 & Claude' })
  assert.equal(oldSignal.aborted, true)
  finish(response([workspace(19)]))
  assert.equal(await stale, false)
  assert.equal(queries[2].get('search'), '旧项目 & Claude')
  assert.equal(queries[2].has('before_id'), false)
  assert.deepEqual(app.conversationIds.value, [1])
  assert.equal(app.conversationNextCursor.value, null)
  assert.equal(app.conversationQuery.value.search, '旧项目 & Claude')
  assert.equal(app.isLoadingConversations.value, false)
  assert.equal(app.conversationError.value, '')
})

test('failed workspace pages retain the cursor and allow retry without duplicate requests', async t => {
  const app = useConversations()
  let finish, calls = 0
  t.mock.method(globalThis, 'fetch', async () => {
    if (++calls === 1) return response([workspace(20)], cursor)
    if (calls === 2) return new Promise(resolve => { finish = resolve })
    return response([workspace(20), workspace(19)])
  })
  await app.fetchConversations()
  const more = app.fetchConversations({ more: true })
  assert.equal(await app.fetchConversations({ more: true }), false)
  finish({ ok: false, text: async () => '暂时不可用' })
  assert.equal(await more, false)
  assert.deepEqual(app.conversationIds.value, [20])
  assert.deepEqual(app.conversationNextCursor.value, cursor)
  assert.equal(app.conversationError.value, '暂时不可用')
  await app.fetchConversations({ more: true })
  assert.deepEqual(app.conversationIds.value, [20, 19])
  assert.equal(app.conversationError.value, '')
  assert.equal(calls, 3)
})

test('a realtime snapshot during load-more preserves the requested depth and rejects the older response', async t => {
  const app = useConversations()
  let finish, calls = 0
  t.mock.method(globalThis, 'fetch', async () => {
    if (++calls === 1) return response([workspace(21), workspace(20)], cursor)
    if (calls === 2) return new Promise(resolve => { finish = resolve })
    return response([workspace(19), workspace(18)])
  })
  await app.fetchConversations()
  const more = app.fetchConversations({ more: true })
  const refresh = app.applyConversations({ conversations: [workspace(22), workspace(21)], next_cursor: { ...cursor, before_id: 21 } })
  assert.deepEqual(app.conversationIds.value, [21, 20]) // Keep the current list while reloading its tail.
  await refresh
  finish(response([workspace(19)]))
  assert.equal(await more, false)
  assert.deepEqual(app.conversationIds.value, [22, 21, 19, 18])
  assert.equal(app.isLoadingConversations.value, false)
})

test('selected old workspaces survive pagination/search, track updates, and disappear only when deleted', async t => {
  const app = useConversations()
  let selected = workspace(1)
  t.mock.method(globalThis, 'fetch', async url => {
    assert.equal(new URL(url, 'http://localhost').searchParams.get('selected_id'), '1')
    return { ok: true, json: async () => ({ conversations: [workspace(200)], next_cursor: cursor, selected_conversation: selected }) }
  })
  await app.fetchConversations({ selectedId: 1 }) // Restore a link to an older conversation.
  app.activeConversationId.value = 1
  assert.equal(app.activeConversation.value.id, 1)
  assert.deepEqual(app.conversationIds.value, [200])
  await app.fetchConversations({ search: 'some other project' })
  assert.equal(app.activeConversation.value.id, 1)
  selected = { ...selected, title: 'renamed' }
  await app.fetchConversations()
  assert.equal(app.activeConversation.value.title, 'renamed')
  selected = null
  await app.fetchConversations()
  assert.equal(app.activeConversation.value, null)
})

test('workspace refreshes defer while the user edits a title', async t => {
  const renaming = ref(false)
  const app = useConversations({ isRenamingConversation: renaming })
  let finish
  t.mock.method(globalThis, 'fetch', () => new Promise(resolve => { finish = resolve }))
  await app.applyConversations({ conversations: [workspace(20)], next_cursor: cursor })
  const pending = app.fetchConversations()
  renaming.value = true
  finish(response([{ ...workspace(20), title: 'stale title' }], cursor))
  assert.equal(await pending, false)
  await app.applyConversations({ conversations: [workspace(19)] })
  assert.deepEqual(app.conversationIds.value, [20])
  assert.equal(app.conversations.value[0].title, '工作 20')
})

test('frequent realtime updates do not starve a refresh of older workspace pages', async t => {
  const app = useConversations()
  let finish, calls = 0
  t.mock.method(globalThis, 'fetch', async () => {
    if (++calls === 1) return response([workspace(20)], cursor)
    if (calls === 3) return new Promise(resolve => { finish = resolve })
    return response([workspace(19)])
  })
  await app.fetchConversations()
  await app.fetchConversations({ more: true })
  const snapshot = title => ({ conversations: [{ ...workspace(20), title }], next_cursor: cursor })
  const pending = app.applyConversations(snapshot('first update'))
  for (let i = 0; i < 10; i++) app.applyConversations(snapshot(`update ${i}`))
  assert.equal(calls, 3)
  finish(response([workspace(19)]))
  await pending
  await new Promise(setImmediate)
  assert.equal(calls, 4)
  assert.deepEqual(app.conversationIds.value, [20, 19])
  assert.equal(app.conversations.value[0].title, 'update 9')
})

test('load-more extends an in-flight background refresh without losing the click', async t => {
  const app = useConversations()
  let finish, calls = 0
  const headCursor = { ...cursor, before_id: 30 }
  t.mock.method(globalThis, 'fetch', async () => {
    if (++calls === 1) return response([workspace(30)], headCursor)
    if (calls === 2) return response([workspace(20)], cursor)
    if (calls === 3) return new Promise(resolve => { finish = resolve })
    return response([workspace(10)])
  })
  await app.fetchConversations()
  await app.fetchConversations({ more: true })
  const refresh = app.applyConversations({ conversations: [workspace(30)], next_cursor: headCursor })
  assert.equal(await app.fetchConversations({ more: true }), true)
  assert.equal(app.isLoadingConversations.value, true)
  assert.equal(await app.fetchConversations({ more: true }), false)
  finish(response([workspace(20)], cursor))
  await refresh
  assert.deepEqual(app.conversationIds.value, [30, 20, 10])
  assert.equal(app.conversationNextCursor.value, null)
  assert.equal(app.isLoadingConversations.value, false)
})


test('archive view resets pagination, rejects stale results, and preserves the selected history', async t => {
  const showingArchivedConversations = ref(false)
  const app = useConversations({ showingArchivedConversations })
  app.activeConversationId.value = 1
  const queries = []
  let finish, oldSignal
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    const query = new URL(url, 'http://localhost').searchParams
    queries.push(query)
    if (queries.length === 1) return response([workspace(1)], cursor)
    if (queries.length === 2) {
      oldSignal = options.signal
      return new Promise(resolve => { finish = resolve })
    }
    return { ok: true, json: async () => ({ conversations: [{ ...workspace(9), is_archived: true }],
      selected_conversation: workspace(1), next_cursor: null }) }
  })
  await app.fetchConversations()
  const stale = app.fetchConversations({ more: true })
  showingArchivedConversations.value = true
  await app.fetchConversations()
  assert.equal(oldSignal.aborted, true)
  finish(response([workspace(20)]))
  await stale
  assert.equal(queries.at(-1).get('archived'), 'true')
  assert.equal(queries.at(-1).has('before_id'), false)
  assert.equal(app.conversationQuery.value.archived, true)
  assert.deepEqual(app.conversationIds.value, [9])
  assert.equal(app.activeConversation.value.id, 1)
})

test('archival persists, prevents duplicate requests, and reloads the filtered list', async t => {
  const app = useConversations()
  app.conversations.value = [workspace(1)]
  app.activeConversationId.value = 1
  let finish
  let patches = 0
  t.mock.method(globalThis, 'fetch', async (_url, options) => {
    if (options?.method === 'PATCH') {
      patches++
      assert.deepEqual(JSON.parse(options.body), { is_archived: true })
      return new Promise(resolve => { finish = resolve })
    }
    return { ok: true, json: async () => ({ conversations: [], selected_conversation: { ...workspace(1), is_archived: true } }) }
  })
  const saving = app.setConversationArchived(1, true)
  assert.deepEqual(app.archivingConversationIds.value, [1])
  assert.equal(await app.setConversationArchived(1, true), false)
  finish({ ok: true })
  assert.equal(await saving, true)
  assert.equal(patches, 1)
  assert.deepEqual(app.conversationIds.value, [])
  assert.equal(app.activeConversation.value.is_archived, true)
  assert.deepEqual(app.archivingConversationIds.value, [])
})

test('archive failures preserve the conversation and show the server explanation', async t => {
  const app = useConversations()
  app.conversations.value = [workspace(1)]
  t.mock.method(globalThis, 'fetch', async () => ({ ok: false, text: async () => JSON.stringify({ error: '当前对话仍有生成任务' }) }))
  await assert.rejects(app.setConversationArchived(1, true), /当前对话仍有生成任务/)
  assert.equal(Boolean(app.conversations.value[0].is_archived), false)
  assert.deepEqual(app.archivingConversationIds.value, [])
})

test('expanding history keeps active conversations and paginates archives with a separate cursor', async t => {
  const app = useConversations({ expandArchivedOnMore: true })
  const queries = []
  t.mock.method(globalThis, 'fetch', async url => {
    const query = new URL(url, 'http://localhost').searchParams
    queries.push(query)
    if (query.get('archived') === 'false') return response([workspace(2)])
    return query.has('before_id')
      ? response([{ ...workspace(7), is_archived: true }])
      : response([{ ...workspace(9), is_archived: true }], { ...cursor, before_id: 9 })
  })
  await app.fetchConversations()
  assert.deepEqual(app.conversationIds.value, [2])
  assert.equal(queries.length, 1)
  assert.equal(app.hasMoreConversations.value, true)
  await app.fetchConversations({ more: true })
  assert.deepEqual(app.conversationIds.value, [2, 9])
  assert.equal(queries[1].get('archived'), 'true')
  assert.equal(queries[1].has('before_id'), false)
  await app.fetchConversations({ more: true })
  assert.deepEqual(app.conversationIds.value, [2, 9, 7])
  assert.equal(queries[2].get('before_id'), '9')
  assert.equal(app.hasMoreConversations.value, false)
  assert.equal(app.conversationQuery.value.archived, false)
  await app.applyArchivedConversations({ conversations: [{ ...workspace(9), is_archived: true, title: '更新的归档' }],
    next_cursor: { ...cursor, before_id: 9 } })
  assert.deepEqual(app.conversationIds.value, [2, 9, 7])
  assert.equal(app.conversations.value.find(row => row.id === 9).title, '更新的归档')
})

test('the first load-more loads archived records even when active records still have more pages', async t => {
  const app = useConversations({ expandArchivedOnMore: true })
  t.mock.method(globalThis, 'fetch', async url => {
    const query = new URL(url, 'http://localhost').searchParams
    if (query.get('archived') === 'true') {
      assert.equal(query.has('before_id'), false)
      return response([{ ...workspace(7), is_archived: true }])
    }
    return query.has('before_id') ? response([workspace(19)]) : response([workspace(20)], cursor)
  })
  await app.fetchConversations()
  await app.fetchConversations({ more: true })
  assert.deepEqual(app.conversationIds.value, [20, 19, 7])
  assert.equal(app.hasMoreConversations.value, false)
})

test('an empty initial list still offers archives and failed expansion can be retried', async t => {
  const app = useConversations({ expandArchivedOnMore: true })
  let fail = true
  t.mock.method(globalThis, 'fetch', async url => {
    if (new URL(url, 'http://localhost').searchParams.get('archived') === 'false') return response([])
    if (fail) return { ok: false, text: async () => '归档暂时不可用' }
    return response([{ ...workspace(7), is_archived: true }])
  })
  await app.fetchConversations()
  assert.equal(app.hasMoreConversations.value, true)
  assert.equal(await app.fetchConversations({ more: true }), false)
  assert.deepEqual(app.conversationIds.value, [])
  assert.equal(app.hasMoreConversations.value, true)
  fail = false
  assert.equal(await app.fetchConversations({ more: true }), true)
  assert.deepEqual(app.conversationIds.value, [7])
  assert.equal(app.hasMoreConversations.value, false)
})

test('changing the search cancels archive expansion and keeps stale archives out of the new list', async t => {
  const showingArchivedConversations = ref(false)
  const app = useConversations({ showingArchivedConversations, expandArchivedOnMore: true })
  let finish, oldSignal
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    const query = new URL(url, 'http://localhost').searchParams
    if (query.get('archived') === 'true') {
      oldSignal = options.signal
      return new Promise(resolve => { finish = resolve })
    }
    return response([workspace(query.get('search') ? 3 : 2)])
  })
  await app.fetchConversations()
  const expansion = app.fetchConversations({ more: true })
  showingArchivedConversations.value = false
  await app.fetchConversations({ search: '新搜索' })
  assert.equal(oldSignal.aborted, true)
  finish(response([{ ...workspace(7), is_archived: true }]))
  assert.equal(await expansion, false)
  assert.deepEqual(app.conversationIds.value, [3])
  assert.equal(app.hasMoreConversations.value, true)
  await app.applyArchivedConversations({ conversations: [{ ...workspace(7), is_archived: true }] })
  assert.deepEqual(app.conversationIds.value, [3])
})

test('active and archive snapshots preserve both loaded partitions during a slow refresh', async t => {
  const app = useConversations({ expandArchivedOnMore: true })
  const archiveCursor = { ...cursor, before_id: 9 }
  const archiveHead = title => ({ conversations: [{ ...workspace(9), is_archived: true, title }], next_cursor: archiveCursor })
  let delayArchiveHead = false, finish
  t.mock.method(globalThis, 'fetch', async url => {
    const query = new URL(url, 'http://localhost').searchParams
    if (query.get('archived') === 'false') return query.has('before_id')
      ? response([workspace(19)]) : response([workspace(20)], cursor)
    if (query.has('before_id')) return response([{ ...workspace(7), is_archived: true }])
    if (delayArchiveHead) return new Promise(resolve => { finish = resolve })
    return response(archiveHead('初始归档').conversations, archiveCursor)
  })
  await app.fetchConversations()
  await app.fetchConversations({ more: true })
  await app.fetchConversations({ more: true })
  delayArchiveHead = true
  const refresh = app.applyConversations({ conversations: [workspace(20)], next_cursor: cursor })
  await new Promise(setImmediate)
  assert.equal(typeof finish, 'function')
  app.applyArchivedConversations(archiveHead('最新归档标题'))
  app.applyConversations({ conversations: [{ ...workspace(20), title: '最新未归档标题' }], next_cursor: cursor })
  finish(response(archiveHead('较旧的归档标题').conversations, archiveCursor))
  await refresh
  await new Promise(setImmediate)
  assert.deepEqual(app.conversationIds.value, [20, 19, 9, 7])
  assert.equal(app.conversations.value.find(row => row.id === 20).title, '最新未归档标题')
  assert.equal(app.conversations.value.find(row => row.id === 9).title, '最新归档标题')
})
