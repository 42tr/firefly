import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createRenderer, ssrContextKey } from 'vue'
import { createServer } from 'vite'

const server = await createServer({ configFile: fileURLToPath(new URL('../../vite.config.ts', import.meta.url)), server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom' })
test.after(() => server.close())
const { default: Input } = await server.ssrLoadModule('/src/components/ChatInputBar.vue')

function setup(t, props = {}) {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  Object.defineProperty(globalThis, 'window', { configurable: true, value: { matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }) } })
  let state, sends = 0
  const { createApp } = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = createApp({ ...Input, setup(props, context) { state = Input.setup(props, context); return state }, render: () => null },
    { modelValue: '检查项目', ...props, onSend: () => { sends += 1 } })
  app.provide(ssrContextKey, { modules: new Set() })
  app.mount({})
  t.after(() => { app.unmount(); if (oldWindow) Object.defineProperty(globalThis, 'window', oldWindow); else delete globalThis.window })
  const key = options => { let prevented = false; state.handleKeydown({ key: 'Enter', preventDefault() { prevented = true }, ...options }); return prevented }
  return { state, key, sends: () => sends }
}

test('scheduled task input uses Enter for newlines and Ctrl/Command Enter for saving', t => {
  const input = setup(t, { submitOnEnter: false, attachmentsEnabled: false, actionLabel: '保存任务', inputPlaceholder: '任务内容' })
  assert.equal(input.key({}), false)
  assert.equal(input.sends(), 0)
  assert.equal(input.key({ ctrlKey: true }), true)
  assert.equal(input.key({ metaKey: true }), true)
  assert.equal(input.sends(), 2)
  assert.equal(input.state.placeholder.value, '任务内容')
})

test('normal chat retains Enter submit while IME, Shift Enter and disabled saves do not submit', t => {
  const input = setup(t)
  input.key({ isComposing: true })
  input.key({ shiftKey: true })
  assert.equal(input.sends(), 0)
  input.key({})
  assert.equal(input.sends(), 1)
})

test('keyboard cannot bypass disabled save or send an empty task', t => {
  const disabled = setup(t, { sendDisabled: true, submitOnEnter: false })
  disabled.key({ ctrlKey: true })
  assert.equal(disabled.sends(), 0)
  const empty = setup(t, { modelValue: '   ', submitOnEnter: false })
  empty.key({ metaKey: true })
  assert.equal(empty.sends(), 0)
})

test('input resizing reserves the composer footprint while measuring', t => {
  const input = setup(t)
  const box = { offsetHeight: 96, style: { minHeight: '' } }
  let measuredWithMinHeight
  const target = { style: { height: '84px' }, closest: () => box }
  Object.defineProperty(target, 'scrollHeight', { get: () => {
    measuredWithMinHeight = box.style.minHeight
    return 84
  } })
  input.state.handleInput({ target })
  assert.equal(measuredWithMinHeight, '96px')
  assert.equal(target.style.height, '84px')
  assert.equal(box.style.minHeight, '')
})
