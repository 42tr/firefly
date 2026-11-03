import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createRenderer, nextTick, ssrContextKey } from 'vue'
import { createServer } from 'vite'
const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { default: Panel } = await server.ssrLoadModule('/src/components/AgentSettingsPanel.vue')
function setup(t, fetcher) {
  t.mock.method(globalThis, 'fetch', fetcher)
  let state
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const component = { ...Panel, setup(props, context) { state = Panel.setup(props, context); return state }, render: () => null }
  const app = createApp(component, { active: true })
  app.provide(ssrContextKey, { modules: new Set() }); app.mount({})
  t.after(() => app.unmount())
  return state
}
const entry = () => ({ id: 'p', name: '新模型配置', base_url: 'https://example.test/v1', api_key: 'key', model: '', context_tokens: null })
const item = (id, size, budget) => ({ id, context_length: size, input_budget: budget })
test('discovers models and chooses the model with its calculated input budget', async t => {
  let request
  const state = setup(t, async (_, options) => { request = JSON.parse(options.body); return { ok: true, json: async () => ({ models: [item('one', 100000, 85500), item('two', null, null)] }) } })
  state.draft.value = { revision: 1, profiles: [entry()], firefly: { id: 'firefly', name: 'Firefly', timeout_secs: 60 }, subagents: [] }; state.selectedProfile.value = 'p'
  await nextTick(); await state.discoverModels()
  assert.equal(request.api_key, 'key')
  assert.equal(state.profile.value.model, 'one')
  assert.equal(state.profile.value.context_tokens, 85500)
  assert.equal(state.profile.value.name, 'example.test')
  assert.deepEqual(state.profile.value.models.map(model => model.id), ['one', 'two'])
  state.profile.value.model = 'two'; state.applyModel()
  assert.equal(state.profile.value.context_tokens, null)
})
test('ignores a stale response and retains the selected model when still available', async t => {
  let finish, count = 0
  const state = setup(t, async () => {
    count++
    if (count === 1) return await new Promise(resolve => { finish = resolve })
    return { ok: true, json: async () => ({ models: [item('one', 32000, 30000), item('two', 64000, 60000)] }) }
  })
  state.draft.value = { revision: 1, profiles: [{ ...entry(), model: 'two' }], firefly: { id: 'firefly', name: 'Firefly', timeout_secs: 60 }, subagents: [] }; state.selectedProfile.value = 'p'
  await nextTick(); const first = state.discoverModels()
  state.profile.value.base_url = 'https://other.test/v1'; state.connectionChanged()
  await new Promise(resolve => setImmediate(resolve))
  finish({ ok: true, json: async () => ({ models: [item('old', 8000, 7600)] }) })
  await first
  assert.equal(state.profile.value.model, 'two')
  assert.equal(state.profile.value.context_tokens, 60000)
  assert.deepEqual(state.models.value.map(model => model.id), ['one', 'two'])
})

test('saved models remain selectable and agents choose models independently', async t => {
  const state = setup(t, async () => ({ ok: true, json: async () => ({ models: [] }) }))
  const savedModels = [item('one', 64000, 60000), item('two', 32000, 30000)]
  state.draft.value = { revision: 1, profiles: [{ ...entry(), model: 'one', models: savedModels }],
    firefly: { id: 'firefly', name: 'Firefly', model_profile: 'p', model: 'two', timeout_secs: 60 },
    subagents: [{ id: 'helper', name: 'helper', model_profile: null, model: null, timeout_secs: 60 }] }
  state.selectedProfile.value = 'p'
  await nextTick()
  assert.deepEqual(state.models.value.map(model => model.id), ['one', 'two'])
  assert.deepEqual(state.agentModels.value.map(model => model.id), ['one', 'two'])
  assert.equal(state.agentModel.value, 'two')
  state.draft.value.firefly.model = null
  assert.equal(state.agentModel.value, 'one')
  state.agentModel.value = 'two'
  assert.equal(state.draft.value.firefly.model, 'two')
  state.tab.value = 'children'; state.selectedAgent.value = 'helper'; await nextTick()
  assert.equal(state.inheritedModel.value, 'two')
  assert.equal(state.agentModel.value, 'two')
  assert.equal(state.agent.value.model, null)
  state.agentModel.value = 'one'
  assert.equal(state.draft.value.firefly.model, 'two')
  assert.equal(state.draft.value.profiles[0].model, 'one')
})

test('agents select only advertised efforts and inherit effort only with the same platform and model', async t => {
  const state = setup(t, async () => ({ ok: true, json: async () => ({ models: [] }) }))
  const models = [
    { id: 'kimi', reasoning_levels: ['low', 'high', 'max'], default_reasoning_level: 'high' },
    { id: 'small', reasoning_levels: ['low', 'high'], default_reasoning_level: 'low' },
    { id: 'unknown', reasoning_levels: null },
  ]
  state.draft.value = { revision: 1, profiles: [{ ...entry(), model: 'kimi', models }],
    firefly: { id: 'firefly', name: 'Firefly', model_profile: 'p', model: null, reasoning_level: 'max', timeout_secs: 60 },
    subagents: [{ id: 'helper', name: 'helper', model_profile: null, model: null, reasoning_level: null, timeout_secs: 60 }] }
  await nextTick()
  assert.deepEqual(state.agentReasoningLevels.value, ['low', 'high', 'max'])
  assert.equal(state.inheritedReasoningLevel.value, 'high')
  assert.equal(state.agentReasoningLevel.value, 'max')
  state.tab.value = 'children'; state.selectedAgent.value = 'helper'; await nextTick()
  assert.equal(state.inheritsRootReasoning.value, true)
  assert.equal(state.inheritedReasoningLevel.value, 'max')
  assert.equal(state.agentReasoningLevel.value, 'max')
  state.agentReasoningLevel.value = 'low'
  assert.equal(state.agent.value.reasoning_level, 'low')
  assert.equal(state.draft.value.firefly.reasoning_level, 'max')
  state.agent.value.model = 'small'; state.agentModelChanged(); await nextTick()
  assert.equal(state.agent.value.reasoning_level, null)
  assert.equal(state.inheritsRootReasoning.value, false)
  assert.deepEqual(state.agentReasoningLevels.value, ['low', 'high'])
  assert.equal(state.inheritedReasoningLevel.value, 'low')
  assert.equal(state.agentReasoningLevel.value, 'low')
  state.agent.value.model = 'unknown'; await nextTick()
  assert.deepEqual(state.agentReasoningLevels.value, [])
  assert.equal(state.inheritedReasoningLevel.value, '')
  assert.equal(state.agentReasoningLevel.value, '')
  state.agent.value.model = 'kimi'; state.agent.value.reasoning_level = 'medium'; await nextTick()
  assert.equal(state.agent.value.reasoning_level, null)
  assert.equal(state.inheritedReasoningLevel.value, 'max')
  assert.equal(state.agentReasoningLevel.value, 'max')
})

test('agent efforts use the profile protocol capability and its documented default', async t => {
  const state = setup(t, async () => ({ ok: true, json: async () => ({ models: [] }) }))
  state.draft.value = { revision: 1, profiles: [{ ...entry(), protocol: 'responses', model: 'qwen3.8-max', models: [{
    id: 'qwen3.8-max', reasoning_levels: null, reasoning_by_protocol: {
      responses: { reasoning_levels: ['none', 'low', 'medium', 'xhigh'], default_reasoning_level: 'xhigh' },
      anthropic: { reasoning_levels: ['low', 'medium', 'xhigh'], default_reasoning_level: 'xhigh' },
    },
  }] }], firefly: { id: 'firefly', name: 'Firefly', model_profile: 'p', model: null, reasoning_level: null, timeout_secs: 60 }, subagents: [] }
  await nextTick()
  assert.deepEqual(state.agentReasoningLevels.value, ['none', 'low', 'medium', 'xhigh'])
  assert.equal(state.inheritedReasoningLevel.value, 'xhigh')
  assert.equal(state.agentReasoningLevel.value, 'xhigh')
  assert.equal(state.agent.value.reasoning_level, null)
  state.agentReasoningLevel.value = 'medium'
  await nextTick()
  assert.equal(state.agent.value.reasoning_level, 'medium')
})

test('deleting a referenced model clears references without deleting another profile', async t => {
  const state = setup(t, async () => ({ ok: true, json: async () => ({ models: [] }) }))
  state.draft.value = { profiles: [entry(), { ...entry(), id: 'other' }], firefly: { model_profile: 'p' }, subagents: [{ model_profile: 'p' }, { model_profile: 'other' }] }
  state.selectedProfile.value = 'p'; await nextTick()
  state.removeProfile()
  assert.deepEqual(state.draft.value.profiles.map(item => item.id), ['other'])
  assert.equal(state.draft.value.firefly.model_profile, null)
  assert.equal(state.draft.value.subagents[0].model_profile, null)
  assert.equal(state.draft.value.subagents[1].model_profile, 'other')
  assert.match(state.notice.value, /重新选择模型/)
})

test('first model can be created after deleting the last one', async t => {
  const state = setup(t, async () => ({ ok: true, json: async () => ({ models: [] }) }))
  state.draft.value = { profiles: [entry()], firefly: { model_profile: 'p' }, subagents: [] }
  state.selectedProfile.value = 'p'; await nextTick()
  state.removeProfile()
  assert.equal(state.draft.value.profiles.length, 0)
  state.addProfile()
  assert.equal(state.draft.value.firefly.model_profile, state.draft.value.profiles[0].id)
})
