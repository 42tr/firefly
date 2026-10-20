import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createRenderer, h, nextTick, ref, ssrContextKey } from 'vue'
import { createServer } from 'vite'

const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { default: Dialog } = await server.ssrLoadModule('/src/components/ModelSettingsDialog.vue')
const listing = {
  default: 'relay', default_model: 'model-a', default_reasoning_level: 'medium',
  providers: [{ name: 'relay', models: ['model-a', 'model-b', 'model-c'] }],
  official_models: ['model-a'],
  model_reasoning: [
    { model: 'model-a', reasoning_levels: ['low', 'medium', 'high'] },
    { model: 'model-b', reasoning_levels: ['low', 'medium'] },
    { model: 'model-c', reasoning_levels: [] },
  ],
}
const saved = { id: 7, agent: 'codex', status: 'completed', codex_provider: 'relay', codex_model: 'model-a', codex_reasoning_level: 'high' }

async function setup(t, {
  conversation = { ...saved },
  patch = async () => ({ ok: true, json: async () => ({ ...saved, codex_provider: 'openai' }) }),
  catalog = async () => ({ ok: true, json: async () => listing }),
} = {}) {
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, 'document')
  Object.defineProperty(globalThis, 'document', { configurable: true, value: { activeElement: { focus() {} } } })
  const requests = []
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    if (options?.method === 'PATCH') { requests.push({ url, payload: JSON.parse(options.body) }); return patch() }
    return catalog()
  })
  let state
  const events = []
  const liveConversation = ref(conversation)
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const component = {
    ...Dialog,
    setup(props, context) { state = Dialog.setup(props, context); return state },
    render: () => null,
  }
  const app = createApp({ render: () => h(component, {
    open: true, conversation: liveConversation.value,
    onSaved: payload => events.push(['saved', payload]), onClose: () => events.push(['close']),
  }) })
  app.provide(ssrContextKey, { modules: new Set() })
  app.mount({})
  t.after(() => {
    app.unmount()
    if (previousDocument) Object.defineProperty(globalThis, 'document', previousDocument)
    else delete globalThis.document
  })
  await new Promise(resolve => setImmediate(resolve))
  await nextTick()
  return { state, events, requests, liveConversation }
}

test('existing settings are preserved and saving official OpenAI overrides a custom global provider', async t => {
  const { state, requests, events } = await setup(t)
  assert.equal(state.provider.value, 'relay')
  assert.equal(state.model.value, 'model-a')
  assert.equal(state.reasoningLevel.value, 'high')
  state.provider.value = ''
  await state.save()
  assert.deepEqual(requests, [{ url: '/api/assistant/conversations/7/codex-settings', payload: { provider: 'openai', model: 'model-a', reasoning_level: 'high' } }])
  assert.equal(events[0][0], 'saved')
  assert.equal(events[0][1].id, 7)
  assert.equal(events[1][0], 'close')
})

test('changing models adjusts supported reasoning and clears effort for a model without reasoning', async t => {
  const { state, requests } = await setup(t)
  state.model.value = 'model-b'
  await nextTick()
  assert.equal(state.reasoningLevel.value, 'medium')
  state.model.value = 'model-c'
  await nextTick()
  assert.equal(state.reasoningLevel.value, '')
  await state.save()
  assert.equal(requests[0].payload.reasoning_level, null)
})

test('inherited Codex settings select and save the resolved platform, model, and effort', async t => {
  const { state, requests } = await setup(t, { conversation: { id: 7, agent: 'codex' } })
  assert.equal(state.provider.value, 'relay')
  assert.equal(state.model.value, 'model-a')
  assert.equal(state.reasoningLevel.value, 'medium')
  await state.save()
  assert.deepEqual(requests[0].payload, { provider: 'relay', model: 'model-a', reasoning_level: 'medium' })
})

test('running and queued conversations can save settings for future messages', async t => {
  for (const status of ['running', 'queued']) {
    const { state, requests } = await setup(t, { conversation: { ...saved, status } })
    assert.equal(state.blockedReason.value, '')
    await state.save()
    assert.equal(requests.length, 1)
  }
})

test('archived conversations still cannot save', async t => {
  const { state, requests } = await setup(t, { conversation: { ...saved, is_archived: true } })
  assert.ok(state.blockedReason.value)
  await state.save()
  assert.equal(requests.length, 0)
})

test('live updates during generation do not reload settings or erase edits in the dialog', async t => {
  let catalogReads = 0
  const { state, requests, liveConversation } = await setup(t, {
    conversation: { ...saved, status: 'running' },
    catalog: async () => { catalogReads += 1; return { ok: true, json: async () => listing } },
  })
  state.model.value = 'model-b'
  await nextTick()
  liveConversation.value = { ...liveConversation.value, message_count: 9 }
  await nextTick()
  assert.equal(state.model.value, 'model-b')
  assert.equal(state.reasoningLevel.value, 'medium')
  assert.equal(catalogReads, 1)
  await state.save()
  assert.equal(requests[0].payload.model, 'model-b')
})

test('a failed catalog cannot silently replace an inherited custom provider with OpenAI', async t => {
  let available = false
  const { state, requests } = await setup(t, {
    conversation: { id: 7, agent: 'codex' },
    catalog: async () => ({ ok: available, json: async () => listing }),
  })
  assert.equal(state.providerResolved.value, false)
  await state.save()
  assert.equal(requests.length, 0)
  available = true
  await state.loadCurrentProviders(state.props.conversation, state.opening)
  assert.equal(state.providerResolved.value, true)
  assert.equal(state.provider.value, 'relay')
})

test('a failed save leaves the chosen settings available for retry and prevents duplicate submissions', async t => {
  let finish
  const { state, requests, events } = await setup(t, { patch: () => new Promise(resolve => { finish = resolve }) })
  const saving = state.save()
  await state.save()
  assert.equal(requests.length, 1)
  finish({ ok: false, text: async () => JSON.stringify({ error: '保存模型设置失败' }) })
  await saving
  assert.match(state.error.value, /保存模型设置失败/)
  assert.equal(state.model.value, 'model-a')
  assert.equal(state.saving.value, false)
  assert.deepEqual(events, [])
})

test('Claude keeps its saved model and updates only Claude settings while a reply is running', async t => {
  const conversation = { id: 8, agent: 'claude', status: 'running', claude_model: 'custom-model' }
  const { state, requests, events } = await setup(t, {
    conversation,
    catalog: async () => ({ ok: true, json: async () => ({ models: ['sonnet', 'opus', 'haiku'], default_model: 'sonnet' }) }),
    patch: async () => ({ ok: true, json: async () => ({ ...conversation, claude_model: 'opus' }) }),
  })
  assert.equal(state.claudeModel.value, 'custom-model')
  assert.deepEqual(state.claudeModelOptions.value, ['sonnet', 'opus', 'haiku'])
  state.claudeModel.value = 'opus'
  await state.save()
  assert.deepEqual(requests, [{ url: '/api/assistant/conversations/8/claude-settings', payload: { provider: 'cli', model: 'opus', reasoning_level: 'medium' } }])
  assert.equal(events[0][1].claude_model, 'opus')
})

test('Claude can restore the inherited model and use manual names when catalog loading fails', async t => {
  const { state, requests } = await setup(t, {
    conversation: { id: 8, agent: 'claude', claude_model: 'opus' },
    catalog: async () => ({ ok: false }),
  })
  assert.ok(state.claudeModelsError.value)
  state.claudeModel.value = ''
  await state.save()
  assert.deepEqual(requests[0].payload, { provider: null, model: null, reasoning_level: null })
})

test('Claude snapshots refresh without overwriting unsaved model changes', async t => {
  let reads = 0
  const { state, requests, liveConversation } = await setup(t, {
    conversation: { id: 8, agent: 'claude', claude_model: 'sonnet', status: 'running' },
    catalog: async () => { reads += 1; return { ok: true, json: async () => ({ models: ['sonnet', 'opus'] }) } },
  })
  state.claudeModel.value = 'opus'
  liveConversation.value = { ...liveConversation.value, message_count: 5 }
  await nextTick()
  assert.equal(reads, 1)
  await state.save()
  assert.deepEqual(requests[0].payload, { provider: 'cli', model: 'opus', reasoning_level: 'medium' })
})

test('Claude saves platform, model, and effort together and changes the model list with its platform', async t => {
  const { state, requests } = await setup(t, {
    conversation: { id: 8, agent: 'claude', claude_provider: 'relay', claude_model: 'relay-opus', claude_reasoning_level: 'max' },
    catalog: async () => ({ ok: true, json: async () => ({
      default: 'relay', models: ['sonnet', 'opus'], cli_default_model: 'sonnet',
      providers: [{ name: 'relay', models: ['relay-sonnet', 'relay-opus'], default_model: 'relay-sonnet' }],
      reasoning_levels: ['low', 'medium', 'high', 'xhigh', 'max'],
    }) }),
  })
  assert.equal(state.claudeProvider.value, 'relay')
  assert.equal(state.claudeReasoningLevel.value, 'max')
  assert.deepEqual(state.claudeModelOptions.value, ['relay-sonnet', 'relay-opus'])
  await state.save()
  assert.deepEqual(requests[0].payload, { provider: 'relay', model: 'relay-opus', reasoning_level: 'max' })
  state.claudeProvider.value = 'cli'
  state.changeClaudeProvider()
  assert.equal(state.claudeModel.value, 'sonnet')
  assert.equal(state.claudeReasoningLevel.value, 'medium')
  assert.deepEqual(state.claudeModelOptions.value, ['sonnet', 'opus'])
})

test('Claude displays each native profile default and keeps valid platforms when another file is invalid', async t => {
  const { state, requests } = await setup(t, {
    conversation: { id: 8, agent: 'claude', claude_provider: 'relay' },
    catalog: async () => ({ ok: true, json: async () => ({
      default: 'cli', models: ['sonnet', 'opus'], cli_default_model: 'opus', default_reasoning_level: 'low',
      providers: [
        { name: 'relay', models: ['custom-model'], default_model: 'custom-model', default_reasoning_level: 'high' },
        { name: 'allowlist', models: ['expensive-model', 'small-model'] },
      ],
      models_error: 'broken.json: Claude settings JSON 格式无效',
    }) }),
  })
  assert.equal(state.claudeModel.value, 'custom-model')
  assert.equal(state.claudeReasoningLevel.value, 'high')
  assert.match(state.claudeModelsError.value, /broken.json/)
  await state.save()
  assert.deepEqual(requests[0].payload, { provider: 'relay', model: 'custom-model', reasoning_level: 'high' })
  state.claudeProvider.value = 'cli'
  state.changeClaudeProvider()
  assert.equal(state.claudeModel.value, 'opus')
  assert.equal(state.claudeReasoningLevel.value, 'low')
  state.claudeProvider.value = 'allowlist'
  state.changeClaudeProvider()
  assert.equal(state.claudeModel.value, 'expensive-model')
  assert.equal(state.claudeReasoningLevel.value, 'medium')
})
