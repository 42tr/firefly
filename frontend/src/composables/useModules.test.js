import assert from 'node:assert/strict'
import test, { after } from 'node:test'
import { createServer } from 'vite'

const server = await createServer({ configFile: 'frontend/vite.config.ts', server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
after(() => server.close())
const { createModulesState } = await server.ssrLoadModule('/src/composables/useModules.js')
const flush = () => new Promise(resolve => setImmediate(resolve))
const module = (id = 'custom', options = {}) => ({ id, enabled: true, version: '1', settings: {}, indicator: { tool: 'status', refresh_secs: 60 }, ...options })

function setup(t) {
  let now = Date.parse('2026-10-13T13:29:00Z')
  const calls = []
  const state = createModulesState((path, options) => new Promise((resolve, reject) => calls.push({ path, options, resolve, reject })), () => now)
  t.after(async () => { state.reset(); calls.forEach(call => call.resolve({})); await flush() })
  const load = async (modules = [module()]) => {
    const pending = state.load()
    calls.at(-1).resolve({ modules })
    await pending
  }
  const indicator = (overrides = {}) => ({ indicator: { label: '自定义提示', icon: 'bell', tone: 'warning',
    starts_at: '2026-10-13T13:30:00Z', expires_at: '2026-10-13T16:00:00Z', ...overrides } })
  return { state, calls, load, indicator, advanceTime: milliseconds => { now += milliseconds },
    setTime: value => { now = Date.parse(value); state.tick() } }
}

test('only enabled modules with declarations are polled and list requests are shared', async t => {
  const { state, calls } = setup(t)
  const first = state.load()
  assert.equal(state.load(), first)
  calls[0].resolve({ modules: [module(), module('disabled', { enabled: false }), module('plain', { indicator: null })] })
  await first
  assert.deepEqual(calls.map(call => call.path), ['', '/custom/indicator'])
  const pending = state.refreshIndicators()
  assert.equal(state.refreshIndicators(), pending)
  calls[1].resolve({ indicator: null }); await pending
  await state.refreshIndicators()
  assert.equal(calls.length, 2)
})

test('generic indicator starts and expires on the client without a network response', async t => {
  const { state, calls, load, indicator, setTime } = setup(t)
  await load()
  calls[1].resolve(indicator()); await flush()
  assert.equal(state.activeIndicators.value.length, 0)
  setTime('2026-10-13T13:30:00Z')
  assert.equal(state.activeIndicators.value[0].module_id, 'custom')
  assert.equal(state.activeIndicators.value[0].icon, 'bell')
  assert.equal(state.indicatorDescription.value, '自定义提示')
  setTime('2026-10-13T16:00:00Z')
  assert.equal(state.activeIndicators.value.length, 0)
  assert.equal(calls.length, 2)
})

test('indicators without a start time appear immediately and remain visible after delayed refreshes', async t => {
  for (const starts_at of [undefined, null]) {
    const { state, calls, load, indicator, advanceTime, setTime } = setup(t)
    await load()
    advanceTime(25)
    calls[1].resolve(indicator({ starts_at })); await flush()
    assert.equal(state.activeIndicators.value.length, 1)
    assert.equal(state.indicatorDescription.value, '自定义提示')

    setTime('2026-10-13T13:30:00Z')
    const pending = state.refreshIndicators()
    assert.equal(calls.length, 3)
    assert.equal(state.activeIndicators.value.length, 1)
    advanceTime(25)
    calls[2].resolve(indicator({ starts_at })); await pending
    assert.equal(state.activeIndicators.value.length, 1)
    assert.equal(state.indicatorDescription.value, '自定义提示')
  }
})

test('delayed responses respect start and expiry boundaries at receipt time', async t => {
  const { state, calls, load, indicator, advanceTime, setTime } = setup(t)
  await load()
  advanceTime(25)
  calls[1].resolve(indicator()); await flush()
  assert.equal(state.activeIndicators.value.length, 0)

  setTime('2026-10-13T13:29:59.990Z')
  const starting = state.invalidateIndicators()
  advanceTime(25)
  calls[2].resolve(indicator()); await starting
  assert.equal(state.activeIndicators.value.length, 1)

  setTime('2026-10-13T15:59:59.990Z')
  const expiring = state.refreshIndicators()
  assert.equal(state.activeIndicators.value.length, 1)
  advanceTime(25)
  calls[3].resolve(indicator()); await expiring
  assert.equal(state.activeIndicators.value.length, 0)
})

test('failed refresh preserves a received indication only until its expiry', async t => {
  const { state, calls, load, indicator, setTime } = setup(t)
  await load()
  calls[1].resolve(indicator()); await flush()
  setTime('2026-10-13T13:30:00Z')
  const pending = state.refreshIndicators()
  calls.at(-1).reject(new Error('offline')); await pending
  assert.equal(state.activeIndicators.value.length, 1)
  setTime('2026-10-13T16:00:00Z')
  assert.equal(state.activeIndicators.value.length, 0)
})

test('null clears an indication immediately', async t => {
  const { state, calls, load, indicator, setTime } = setup(t)
  await load()
  calls[1].resolve(indicator()); await flush()
  setTime('2026-10-13T13:30:00Z')
  const pending = state.refreshIndicators()
  calls.at(-1).resolve({ indicator: null }); await pending
  assert.equal(state.activeIndicators.value.length, 0)
})

test('invalid labels, timestamps and excessively long expiry windows are ignored', async t => {
  const { state, calls, load, indicator, setTime } = setup(t)
  setTime('2026-10-13T13:30:00Z')
  for (const overrides of [{ label: '' }, { label: 'x'.repeat(201) }, { starts_at: 'invalid' },
    { expires_at: 'invalid' }, { expires_at: '2026-10-13T13:29:59Z' }, { expires_at: '2026-10-15T00:00:00Z' }]) {
    await load()
    calls.at(-1).resolve(indicator(overrides)); await flush()
    assert.equal(state.activeIndicators.value.length, 0)
  }
})

test('presentation falls back to supported values without evaluating module markup', async t => {
  const { state, calls, load, indicator, setTime } = setup(t)
  await load()
  calls.at(-1).resolve(indicator({ icon: '<script>', tone: 'arbitrary-css', starts_at: null })); await flush()
  setTime('2026-10-13T13:30:00Z')
  const value = state.activeIndicators.value[0]
  assert.equal(value.icon, 'activity')
  assert.equal(value.tone, 'info')
})

test('logout aborts in-flight requests and late results cannot restore state', async t => {
  const { state, calls, load, indicator, setTime } = setup(t)
  await load()
  const old = calls[1]
  state.reset()
  assert.equal(old.options.signal.aborted, true)
  old.resolve(indicator()); await flush()
  setTime('2026-10-13T13:30:00Z')
  assert.deepEqual(state.modules.value, [])
  assert.deepEqual(state.activeIndicators.value, [])
  const loading = state.load()
  state.reset()
  calls.at(-1).resolve({ modules: [module()] }); await loading
  assert.deepEqual(state.modules.value, [])
  assert.equal(state.loading.value, false)
})

test('disabling a module discards its old in-flight indication', async t => {
  const { state, calls, load, indicator, setTime } = setup(t)
  await load()
  const old = calls[1]
  const saving = state.save('custom', { enabled: false, settings: {} })
  assert.equal(calls[2].options.method, 'PUT')
  calls[2].resolve({ modules: [module('custom', { enabled: false })] }); await saving
  old.resolve(indicator()); await flush()
  setTime('2026-10-13T13:30:00Z')
  assert.deepEqual(state.activeIndicators.value, [])
  assert.equal(calls.length, 3)
})

test('changing settings removes an old indication while fetching the new one', async t => {
  const { state, calls, load, indicator, setTime } = setup(t)
  await load()
  calls[1].resolve(indicator()); await flush()
  setTime('2026-10-13T13:30:00Z')
  assert.equal(state.activeIndicators.value.length, 1)
  const saving = state.save('custom', { enabled: true, settings: { site: 'new' } })
  calls[2].resolve({ modules: [module('custom', { settings: { site: 'new' } })] }); await saving
  assert.deepEqual(state.activeIndicators.value, [])
  assert.equal(calls[3].path, '/custom/indicator')
  calls[3].resolve(indicator({ label: '新地点的提示' })); await flush()
  assert.equal(state.indicatorDescription.value, '新地点的提示')
})

test('a save supersedes a list request without leaving loading stuck', async t => {
  const { state, calls } = setup(t)
  const loading = state.load()
  const saving = state.save('custom', { enabled: false })
  calls[1].resolve({ modules: [module('custom', { enabled: false })] }); await saving
  calls[0].resolve({ modules: [module()] }); await loading
  assert.equal(state.modules.value[0].enabled, false)
  assert.equal(state.loading.value, false)
})

test('background indicator calls use at most four concurrent requests', async t => {
  const { state, calls, load } = setup(t)
  await load(Array.from({ length: 9 }, (_, index) => module('extension-' + index)))
  assert.equal(calls.length, 5)
  const pending = state.refreshIndicators()
  for (let index = 1; index <= 9; index++) {
    assert.ok(calls.length <= index + 4)
    calls[index].resolve({ indicator: null }); await flush()
  }
  await pending
  assert.equal(calls.length, 10)
})

test('a module write refreshes indicators after an older in-flight snapshot completes', async t => {
  const { state, calls, load, indicator } = setup(t)
  await load()
  const invalidated = state.invalidateIndicators()
  calls[1].resolve(indicator())
  await flush()
  assert.equal(calls[2].path, '/custom/indicator')
  calls[2].resolve({ indicator: null })
  await invalidated
  assert.equal(state.activeIndicators.value.length, 0)
})
