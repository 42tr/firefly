import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createServer } from 'vite'

const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
test.after(() => server.close())
const { useAgentSettings, settingsPayload, portableSettings } = await server.ssrLoadModule('/src/composables/useAgentSettings.js')
const fixture = () => ({ revision: 3,
  profiles: [{ id: 'default', name: '默认', base_url: 'https://example.com/v1', model: 'model-a', api_key: null, key_configured: true, context_tokens: null }],
  firefly: { id: 'firefly', name: 'Firefly', description: '主助手', instruction: 'main', model_profile: 'default', tools: ['command'], enabled: true, timeout_secs: 86400 },
  subagents: [],
})

test('save preserves masked credentials and clears them only when explicitly requested', () => {
  const settings = fixture()
  assert.equal(settingsPayload(settings).profiles[0].api_key, null)
  settings.profiles[0].clear_key = true
  assert.equal(settingsPayload(settings).profiles[0].api_key, '')
  settings.profiles[0].clear_key = false
  settings.profiles[0].api_key = 'new-private-key'
  assert.equal(settingsPayload(settings).profiles[0].api_key, 'new-private-key')
  assert.equal('key_configured' in settingsPayload(settings).profiles[0], false)
  assert.equal('clear_key' in settingsPayload(settings).profiles[0], false)
})

test('failed and conflicting saves retain the draft; successful saves use the new revision', async () => {
  let fail = true, submitted
  const state = useAgentSettings(async (path, options) => {
    if (options?.method === 'PUT') {
      submitted = JSON.parse(options.body)
      if (fail) throw new Error('配置已被其他页面修改')
      return { ...submitted, revision: 4 }
    }
    return path ? { tools: [{ name: 'command' }] } : fixture()
  })
  await state.load()
  state.draft.value.firefly.instruction = 'updated'
  assert.equal(await state.save(), false)
  assert.equal(state.draft.value.firefly.instruction, 'updated')
  assert.equal(state.draft.value.revision, 3)
  assert.equal(state.dirty.value, true)
  assert.match(state.error.value, /其他页面/)
  fail = false
  assert.equal(await state.save(), true)
  assert.equal(state.draft.value.revision, 4)
  assert.equal(state.dirty.value, false)
  assert.equal(submitted.firefly.instruction, 'updated')
})

test('portable export removes even newly typed keys; imports remain unsaved and use the current revision', async () => {
  const state = useAgentSettings(async path => path ? { tools: [] } : fixture())
  await state.load()
  state.draft.value.profiles[0].api_key = 'new-private-key'
  const exported = portableSettings(state.draft.value)
  assert.equal(JSON.stringify(exported).includes('new-private-key'), false)
  assert.equal(exported.profiles[0].api_key, null)
  exported.subagents = [{ id: 'helper', name: '助手', description: '整理信息', instruction: 'child', tools: [], model_profile: null, enabled: true, timeout_secs: 60 }]
  state.importConfig(exported)
  assert.equal(state.draft.value.revision, 3)
  assert.equal(state.draft.value.subagents[0].id, 'helper')
  assert.equal(state.dirty.value, true)
  assert.throws(() => state.importConfig({}), /导出/)
})

test('refreshing module tools does not discard unsaved agent edits', async () => {
  const state = useAgentSettings(async path => path ? { tools: [{ name: 'mcp__custom__lookup' }] } : fixture())
  await state.load()
  state.draft.value.firefly.instruction = 'unsaved'
  await state.refreshTools()
  assert.equal(state.tools.value[0].name, 'mcp__custom__lookup')
  assert.equal(state.draft.value.firefly.instruction, 'unsaved')
})
