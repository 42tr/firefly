import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test from 'node:test'
import { nextTick } from 'vue'
import { createServer } from 'vite'
const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
test.after(() => server.close())
const { useAgentSettings, settingsPayload } = await server.ssrLoadModule('/src/composables/useAgentSettings.js')
const fixture = () => ({ revision: 3,
  profiles: [{ id: 'default', name: '默认', base_url: 'https://example.com/v1', model: 'model-a', api_key: null, key_configured: true, context_tokens: null }],
  firefly: { id: 'firefly', name: 'Firefly', description: '主助手', instruction: 'main', model_profile: 'default', tools: ['command'], enabled: true, timeout_secs: 86400 }, subagents: [],
})
const response = (body, revision) => ({ ...body, revision, profiles: body.profiles.map(p => ({ ...p, api_key: null, key_configured: p.api_key !== '' })) })
const delay = ms => new Promise(resolve => setTimeout(resolve, ms))
test('payload preserves masked credentials and clears only on explicit request', () => {
  const settings = fixture(); assert.equal(settingsPayload(settings).profiles[0].api_key, null)
  settings.profiles[0].clear_key = true; assert.equal(settingsPayload(settings).profiles[0].api_key, '')
  settings.profiles[0].clear_key = false; settings.profiles[0].api_key = 'new-private-key'
  assert.equal(settingsPayload(settings).profiles[0].api_key, 'new-private-key'); assert.equal('key_configured' in settingsPayload(settings).profiles[0], false)
})
test('autosave persists discovered effort capabilities with each model', async () => {
  let submitted
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') { submitted = JSON.parse(options.body); return response(submitted, 4) }
    return path ? { tools: [] } : fixture()
  }, { autoSave: false })
  await state.load()
  state.draft.value.profiles[0].models = [{ id: 'k3.1-pro', context_length: 1048576, input_budget: 996148,
    reasoning_levels: ['low', 'high', 'max'], default_reasoning_level: 'high' }]
  assert.equal(await state.save(), true)
  assert.deepEqual(submitted.profiles[0].models[0].reasoning_levels, ['low', 'high', 'max'])
  assert.equal(submitted.profiles[0].models[0].default_reasoning_level, 'high')
  assert.equal(state.dirty.value, false)
})
test('autosave persists independent main and child efforts including restoring inheritance', async () => {
  const submitted = []
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') { const body = JSON.parse(options.body); submitted.push(body); return response(body, body.revision + 1) }
    return path ? { tools: [] } : fixture()
  }, { autoSave: false })
  await state.load()
  state.draft.value.firefly.reasoning_level = 'max'
  state.draft.value.subagents.push({ ...state.draft.value.firefly, id: 'helper', description: 'helper', model_profile: null, reasoning_level: 'low' })
  assert.equal(await state.save(), true)
  assert.equal(submitted[0].firefly.reasoning_level, 'max')
  assert.equal(submitted[0].subagents[0].reasoning_level, 'low')
  state.draft.value.subagents[0].reasoning_level = null
  assert.equal(await state.save(), true)
  assert.equal(submitted[1].subagents[0].reasoning_level, null)
  assert.equal(state.dirty.value, false)
})
test('edits automatically save after a debounce', async () => {
  const submitted = []; const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') { const body = JSON.parse(options.body); submitted.push(body); return response(body, 4) }
    return path ? { tools: [] } : fixture()
  }, { debounceMs: 10 })
  await state.load(); state.draft.value.firefly.instruction = 'new instruction'; await delay(40)
  assert.equal(submitted.length, 1); assert.equal(submitted[0].firefly.instruction, 'new instruction'); assert.equal(state.draft.value.revision, 4); assert.equal(state.dirty.value, false)
})
test('serializes changes during a save and preserves newer key and edits', async () => {
  const submitted = []; let release
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') { const body = JSON.parse(options.body); submitted.push(body); if (submitted.length === 1) return await new Promise(resolve => { release = () => resolve(response(body, 4)) }); return response(body, 5) }
    return path ? { tools: [] } : fixture()
  }, { autoSave: false })
  await state.load(); state.draft.value.profiles[0].api_key = 'first-key'; const first = state.save(); await nextTick()
  state.draft.value.firefly.instruction = 'later edit'; state.draft.value.profiles[0].api_key = 'second-key'; release(); assert.equal(await first, true)
  assert.equal(submitted.length, 2); assert.equal(submitted[1].revision, 4); assert.equal(submitted[1].profiles[0].api_key, 'second-key'); assert.equal(submitted[1].firefly.instruction, 'later edit'); assert.equal(state.draft.value.profiles[0].api_key, null); assert.equal(state.dirty.value, false)
})
test('masked key is not resent after another field changes while saving', async () => {
  const submitted = []; let release
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') { const body = JSON.parse(options.body); submitted.push(body); if (submitted.length === 1) return await new Promise(resolve => { release = () => resolve(response(body, 4)) }); return response(body, 5) }
    return path ? { tools: [] } : fixture()
  }, { autoSave: false })
  await state.load(); state.draft.value.profiles[0].api_key = 'new-key'; const first = state.save(); await nextTick(); state.draft.value.firefly.instruction = 'edited'; release()
  assert.equal(await first, true); assert.equal(submitted.length, 2); assert.equal(submitted[1].profiles[0].api_key, null); assert.equal(state.dirty.value, false)
})
test('invalid or failed saves retain edits and allow retry', async () => {
  let fail = true, count = 0
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') { count++; if (fail) throw new Error('配置已被其他页面修改'); return response(JSON.parse(options.body), 4) }
    return path ? { tools: [] } : fixture()
  }, { autoSave: false })
  await state.load(); state.draft.value.profiles[0].base_url = 'not a URL'
  assert.equal(await state.save(), false); assert.equal(count, 0); assert.match(state.error.value, /服务地址/)
  state.draft.value.profiles[0].base_url = 'https://example.com/v1'; state.draft.value.firefly.instruction = 'new'
  assert.equal(await state.save(), false); assert.equal(state.draft.value.firefly.instruction, 'new'); assert.match(state.error.value, /其他页面/)
  fail = false; assert.equal(await state.save(), true); assert.equal(state.dirty.value, false)
})
test('refreshing module tools preserves edits', async () => {
  const state = useAgentSettings(async path => path ? { tools: [{ name: 'mcp__custom__lookup' }] } : fixture(), { autoSave: false })
  await state.load(); state.draft.value.firefly.instruction = 'unsaved'; await state.refreshTools()
  assert.equal(state.tools.value[0].name, 'mcp__custom__lookup'); assert.equal(state.draft.value.firefly.instruction, 'unsaved')
})

test('saving preserves the active profile object for an in-flight model lookup', async () => {
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') return response(JSON.parse(options.body), 4)
    return path ? { tools: [] } : fixture()
  }, { autoSave: false })
  await state.load(); const profile = state.draft.value.profiles[0]
  profile.api_key = 'new-key'; assert.equal(await state.save(), true)
  assert.equal(state.draft.value.profiles[0], profile)
  assert.equal(profile.api_key, null); assert.equal(profile.key_configured, true)
})

test('waiting for an in-flight save flushes subsequent edits before resolving', async () => {
  const submitted = []; let release
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') { const body = JSON.parse(options.body); submitted.push(body); if (submitted.length === 1) return await new Promise(resolve => { release = () => resolve(response(body, 4)) }); return response(body, 5) }
    return path ? { tools: [] } : fixture()
  }, { autoSave: false })
  await state.load(); state.draft.value.firefly.instruction = 'first'; const first = state.save()
  state.draft.value.firefly.instruction = 'second'; const closing = state.save(); release()
  assert.equal(await first, true); assert.equal(await closing, true)
  assert.equal(submitted.length, 2); assert.equal(submitted[1].firefly.instruction, 'second'); assert.equal(state.dirty.value, false)
})

test('a failed reload after a conflict keeps the local draft intact', async () => {
  let failRead = false
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') throw new Error('配置已被其他页面修改')
    if (failRead) throw new Error('无法读取')
    return path ? { tools: [] } : fixture()
  }, { autoSave: false })
  await state.load(); state.draft.value.firefly.instruction = 'local edit'
  assert.equal(await state.save(), false)
  assert.equal(await state.load(), false)
  failRead = true; assert.equal(await state.load({ discardChanges: true }), false)
  assert.equal(state.draft.value.firefly.instruction, 'local edit')
  failRead = false; assert.equal(await state.load({ discardChanges: true }), true)
  assert.equal(state.draft.value.firefly.instruction, 'main')
})
