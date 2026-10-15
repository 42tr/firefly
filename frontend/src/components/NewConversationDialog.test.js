import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createRenderer, nextTick, ssrContextKey } from 'vue'
import { createServer } from 'vite'

const server = await createServer({ configFile: 'frontend/vite.config.ts', server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { default: Dialog } = await server.ssrLoadModule('/src/components/NewConversationDialog.vue')

const listing = {
  default: 'relay', default_model: 'model-a', default_reasoning_level: 'high',
  providers: [{ name: 'relay', models: ['model-a', 'model-b', 'model-c'] }],
  official_models: ['model-a', 'model-b', 'model-c'],
  model_reasoning: [
    { model: 'model-a', reasoning_levels: ['low', 'medium', 'high', 'xhigh'], default_reasoning_level: 'medium' },
    { model: 'model-b', reasoning_levels: ['low', 'medium'], default_reasoning_level: 'low' },
    { model: 'model-c', reasoning_levels: [], default_reasoning_level: null },
  ],
}

function setup(t, data = listing, defaultAgent = 'codex') {
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, 'document')
  Object.defineProperty(globalThis, 'document', { configurable: true, value: { addEventListener() {}, removeEventListener() {} } })
  t.mock.method(globalThis, 'fetch', async () => ({ ok: true, json: async () => data }))
  const events = []
  let state
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = createApp({
    ...Dialog,
    setup(props, context) {
      state = Dialog.setup(props, context)
      return state
    },
    render: () => null,
  }, { open: false, creating: false, defaultAgent, onChange: payload => events.push({ name: 'change', payload }) })
  app.provide(ssrContextKey, { modules: new Set() })
  app.mount({})
  t.after(() => {
    app.unmount()
    if (previousDocument) Object.defineProperty(globalThis, 'document', previousDocument)
    else delete globalThis.document
  })
  return { state, events }
}

test('Codex and Claude keep separate reasoning selections', async t => {
  const { state, events } = setup(t)
  await state.loadProviders()
  assert.equal(state.model.value, 'model-a')
  assert.equal(state.reasoningLevel.value, 'high')
  state.reasoningLevel.value = 'xhigh'
  await nextTick()
  assert.equal(events.at(-1).payload.reasoning_level, 'xhigh')
  state.agent.value = 'claude'
  await nextTick()
  assert.equal(state.selection.value.reasoning_level, '')
})

test('changes available levels with the model and clears unsupported reasoning', async t => {
  const { state } = setup(t)
  await state.loadProviders()
  state.reasoningLevel.value = 'xhigh'
  state.model.value = 'model-b'
  await nextTick()
  assert.deepEqual(state.reasoningOptions.value, ['low', 'medium'])
  assert.equal(state.reasoningLevel.value, 'low')
  state.model.value = 'model-c'
  await nextTick()
  assert.deepEqual(state.reasoningOptions.value, [])
  assert.equal(state.reasoningLevel.value, '')
})

test('preserves a saved level and resolves provider-prefixed model names', async t => {
  const { state } = setup(t)
  state.provider.value = 'relay'
  state.model.value = 'relay/model-a'
  state.reasoningLevel.value = 'xhigh'
  await state.loadProviders(false)
  assert.equal(state.reasoningLevel.value, 'xhigh')
  assert.deepEqual(state.reasoningOptions.value, listing.model_reasoning[0].reasoning_levels)
})

test('uses standard choices when model capabilities are unavailable', async t => {
  const { state } = setup(t, { official_models: ['custom-model'] })
  await state.loadProviders()
  assert.deepEqual(state.reasoningOptions.value, ['low', 'medium', 'high', 'xhigh'])
  assert.equal(state.reasoningLevel.value, 'medium')
})

test('surfaces a failed provider model lookup while keeping manual entry', async t => {
  const message = '读取平台模型列表失败，可手动输入模型名称或使用默认模型'
  const { state } = setup(t, {
    default: 'kimi',
    providers: [{ name: 'kimi', base_url: 'https://api.kimi.com/v1', models: [], models_error: message }],
    official_models: [],
  })
  await state.loadProviders()
  assert.equal(state.provider.value, 'kimi')
  assert.deepEqual(state.modelOptions.value, [])
  assert.equal(state.providerModelsError.value, message)
  state.provider.value = ''
  await nextTick()
  assert.equal(state.providerModelsError.value, '')
})

test('stays quiet when a provider merely returns an empty model list', async t => {
  const { state } = setup(t, {
    default: 'relay',
    providers: [{ name: 'relay', models: [] }],
    official_models: [],
  })
  await state.loadProviders()
  assert.equal(state.provider.value, 'relay')
  assert.deepEqual(state.modelOptions.value, [])
  assert.equal(state.providerModelsError.value, '')
})

test('keeps configured and saved levels when the catalog is unavailable', async t => {
  const { state } = setup(t, { default_reasoning_level: 'max', official_models: ['custom-model'] })
  await state.loadProviders()
  assert.equal(state.reasoningLevel.value, 'max')
  state.reasoningLevel.value = 'ultra'
  await state.loadProviders(false)
  assert.equal(state.reasoningLevel.value, 'ultra')
  assert.ok(state.reasoningOptions.value.includes('ultra'))
})

test('Firefly selection excludes workspace and provider settings', async t => {
  const { state, events } = setup(t, listing, 'firefly')
  state.parallel.value = true
  state.path.value = 'old-workspace'
  await nextTick()
  assert.deepEqual(state.selection.value, {
    agent: 'firefly', workspace_mode: 'shared', cwd: null,
    provider: null, model: null, reasoning_level: null,
  })
})

test('Claude creation emits its selected model without Codex provider and reasoning fields', async t => {
  const { state } = setup(t, { models: ['sonnet', 'opus', 'haiku'], default_model: 'sonnet' }, 'claude')
  await state.loadClaudeModels()
  state.claudeModel.value = 'opus'
  state.provider.value = 'relay'
  state.reasoningLevel.value = 'high'
  assert.equal(state.selection.value.model, 'opus')
  assert.equal(state.selection.value.provider, '')
  assert.equal(state.selection.value.reasoning_level, '')
  assert.deepEqual(state.claudeModelOptions.value, ['sonnet', 'opus', 'haiku'])
  state.applyWorkspace({ agent: 'claude', cwd: 'project', claude_model: 'custom-model' })
  await nextTick()
  assert.equal(state.selection.value.model, 'custom-model')
})

test('Claude creation and workspace presets retain their own platform, model, and effort', async t => {
  const { state, events } = setup(t, {
    default: 'relay', cli_default_model: 'sonnet', models: ['sonnet', 'opus'],
    providers: [{ name: 'relay', models: ['custom-sonnet', 'custom-opus'], default_model: 'custom-opus' }],
  }, 'claude')
  await state.loadClaudeModels()
  assert.deepEqual(state.claudeModelOptions.value, ['custom-sonnet', 'custom-opus'])
  state.claudeProvider.value = 'relay'
  state.claudeModel.value = 'custom-opus'
  state.claudeReasoningLevel.value = 'max'
  await nextTick()
  assert.deepEqual(events.at(-1).payload, {
    agent: 'claude', workspace_mode: 'shared', cwd: '', provider: 'relay', model: 'custom-opus', reasoning_level: 'max',
  })
  state.applyWorkspace({ agent: 'claude', cwd: 'project', claude_provider: 'cli', claude_model: 'opus', claude_reasoning_level: 'low' })
  assert.equal(state.selection.value.provider, 'cli')
  assert.equal(state.selection.value.reasoning_level, 'low')
  state.claudeProvider.value = 'relay'
  state.changeClaudeProvider()
  assert.equal(state.selection.value.model, '')
  state.resetPicker()
  assert.equal(state.selection.value.provider, '')
  assert.equal(state.selection.value.reasoning_level, '')
})

test('switching to Firefly clears a pending directory request and ignores its late failure', async t => {
  const { state, events } = setup(t)
  let finish
  t.mock.method(globalThis, 'fetch', () => new Promise(resolve => { finish = resolve }))
  const loading = state.loadWorkspace()
  assert.equal(state.isLoading.value, true)
  state.changeAgent('firefly')
  assert.equal(state.isLoading.value, false)
  await nextTick()
  assert.equal(state.selection.value.agent, 'firefly')
  finish({ ok: false, text: async () => 'directory unavailable' })
  await loading
  assert.equal(state.error.value, '')
})

test('directory browsing updates the selection only after confirming the chosen basename', async t => {
  const { state } = setup(t)
  assert.equal(state.directoryLabel.value, 'workspace')
  state.pane.value = 'workspace'
  state.path.value = 'projects/firefly'
  assert.equal(state.selection.value.cwd, '')
  state.confirmPane()
  assert.equal(state.selection.value.cwd, 'projects/firefly')
  assert.equal(state.directoryLabel.value, 'firefly')
  assert.equal(state.directoryTitle.value, 'workspace/projects/firefly')
  state.pane.value = 'workspace'
  state.path.value = 'projects/another-project'
  state.close()
  assert.equal(state.selection.value.cwd, 'projects/firefly')
})

test('confirmed directory remains intact while the user keeps browsing', async t => {
  const { state } = setup(t, listing, 'claude')
  state.pane.value = 'workspace'
  state.path.value = 'projects/app'
  state.confirmPane()
  assert.equal(state.selection.value.cwd, 'projects/app')
  assert.equal(state.directoryLabel.value, 'app')
  state.path.value = ''
  assert.equal(state.selection.value.cwd, 'projects/app')
  state.changeAgent('firefly')
  await nextTick()
  assert.equal(state.selection.value.cwd, null)
})

test('closing the directory picker cancels its pending listing and ignores a late error', async t => {
  const { state } = setup(t)
  let finish
  t.mock.method(globalThis, 'fetch', () => new Promise(resolve => { finish = resolve }))
  state.pane.value = 'workspace'
  const loading = state.loadWorkspace()
  assert.equal(state.busy.value, true)
  state.close()
  assert.equal(state.busy.value, false)
  finish({ ok: false, text: async () => 'unavailable' })
  await loading
  assert.equal(state.error.value, '')
  assert.equal(state.selection.value.cwd, '')
})


test('inline configuration applies changes immediately and preserves them after collapse', async t => {
  const { state, events } = setup(t)
  await state.loadProviders()
  await state.openPane('settings')
  state.model.value = 'model-b'
  state.parallel.value = true
  await nextTick()
  assert.equal(events.at(-1).payload.model, 'model-b')
  assert.equal(events.at(-1).payload.workspace_mode, 'parallel')
  await state.openPane('settings')
  assert.equal(state.pane.value, null)
  await state.openPane('settings')
  assert.equal(state.model.value, 'model-b')
  assert.equal(state.reasoningLevel.value, 'low')
  assert.equal(state.parallel.value, true)
})

test('Escape collapses inline settings while composition keeps them open', async t => {
  const { state } = setup(t)
  await state.openPane('settings')
  let prevented = 0
  let stopped = 0
  const event = { key: 'Escape', isComposing: true,
    preventDefault() { prevented += 1 }, stopPropagation() { stopped += 1 } }
  state.settingsKeydown(event)
  assert.equal(state.pane.value, 'settings')
  assert.equal(prevented, 0)
  state.settingsKeydown({ ...event, isComposing: false })
  assert.equal(state.pane.value, null)
  assert.equal(prevented, 1)
  assert.equal(stopped, 1)
})

test('switching from inline workspace to settings ignores the abandoned listing', async t => {
  const { state } = setup(t)
  let finish
  t.mock.method(globalThis, 'fetch', () => new Promise(resolve => { finish = resolve }))
  state.pane.value = 'workspace'
  const loading = state.loadWorkspace()
  await state.openPane('settings')
  assert.equal(state.pane.value, 'settings')
  assert.equal(state.busy.value, false)
  finish({ ok: false, text: async () => 'directory unavailable' })
  await loading
  assert.equal(state.error.value, '')
  assert.equal(state.selection.value.cwd, '')
})
