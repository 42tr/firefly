import assert from 'node:assert/strict'
import test from 'node:test'
import { createRenderer } from 'vue'
import { useMobileViewport } from './useMobileViewport.js'

function mount(t, { width = 390, height = 780, withViewport = true } = {}) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const viewport = Object.assign(new EventTarget(), { height, offsetTop: 0, scale: 1 })
  const frames = new Map()
  let frameId = 0
  const browser = Object.assign(new EventTarget(), {
    innerWidth: width, innerHeight: height,
    visualViewport: withViewport ? viewport : null,
    requestAnimationFrame(callback) { frames.set(++frameId, callback); return frameId },
    cancelAnimationFrame(id) { frames.delete(id) },
  })
  Object.defineProperty(globalThis, 'window', { configurable: true, value: browser })
  const flush = () => {
    const pending = [...frames.values()]
    frames.clear()
    pending.forEach(callback => callback())
  }
  let state
  const renderer = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
  const app = renderer.createApp({ setup() { state = useMobileViewport(); return () => null } })
  app.mount({})
  t.after(() => {
    app.unmount()
    if (previous) Object.defineProperty(globalThis, 'window', previous)
    else delete globalThis.window
  })
  return { browser, viewport, frames, flush, app, style: state.viewportStyle }
}

test('keeps the shell inside the keyboard-visible viewport even when Safari pans it', t => {
  const { viewport, flush, style } = mount(t)
  viewport.height = 340
  viewport.dispatchEvent(new Event('resize'))
  flush()
  viewport.offsetTop = 440
  viewport.dispatchEvent(new Event('scroll'))
  flush()
  assert.equal(style.value.position, 'fixed')
  assert.equal(style.value.top, '440px')
  assert.equal(style.value.height, '340px')
  assert.equal(style.value['--app-viewport-height'], '340px')
  assert.equal(style.value['--composer-bottom-inset'], '0px')
  // In visual coordinates, the shell still runs from 0 to 340, not -440 to -100.
  assert.equal(parseFloat(style.value.top) - viewport.offsetTop, 0)
  assert.equal(parseFloat(style.value.top) + parseFloat(style.value.height) - viewport.offsetTop, 340)
})

test('keeps keyboard geometry until dismissal completes and restores the full viewport afterward', t => {
  const { browser, viewport, flush, style } = mount(t)
  viewport.height = 360
  viewport.offsetTop = 420
  viewport.dispatchEvent(new Event('resize'))
  flush()
  // A window resize during focus transfer must not restore the layout height.
  browser.dispatchEvent(new Event('resize'))
  flush()
  assert.equal(style.value.height, '360px')
  viewport.height = 780
  viewport.offsetTop = 0
  viewport.dispatchEvent(new Event('resize'))
  viewport.dispatchEvent(new Event('scroll'))
  flush()
  assert.equal(style.value.top, '0px')
  assert.equal(style.value.height, '780px')
  assert.equal(style.value['--composer-bottom-inset'], 'env(safe-area-inset-bottom)')
})

test('preserves native pinch zoom and resumes tracking at normal scale', t => {
  const { viewport, flush, style } = mount(t)
  viewport.scale = 2
  viewport.height = 390
  viewport.dispatchEvent(new Event('resize'))
  flush()
  assert.equal(style.value, null)
  viewport.scale = 1
  viewport.height = 780
  viewport.dispatchEvent(new Event('resize'))
  flush()
  assert.equal(style.value.height, '780px')
})

test('uses the CSS layout on desktop and follows breakpoint changes', t => {
  const { browser, viewport, flush, style } = mount(t, { width: 1440, height: 1024 })
  assert.equal(style.value, null)
  browser.innerWidth = 390
  browser.innerHeight = 780
  viewport.height = 780
  browser.dispatchEvent(new Event('resize'))
  flush()
  assert.equal(style.value.height, '780px')
  browser.innerWidth = 1440
  browser.dispatchEvent(new Event('resize'))
  flush()
  assert.equal(style.value, null)
})

test('falls back to CSS when VisualViewport is unavailable', t => {
  const { browser, flush, style } = mount(t, { withViewport: false })
  browser.dispatchEvent(new Event('resize'))
  flush()
  assert.equal(style.value, null)
})

test('coalesces viewport events and cancels pending work and listeners on unmount', t => {
  const { browser, viewport, frames, app, flush, style } = mount(t)
  viewport.height = 350
  viewport.dispatchEvent(new Event('resize'))
  viewport.dispatchEvent(new Event('scroll'))
  browser.dispatchEvent(new Event('resize'))
  assert.equal(frames.size, 1)
  app.unmount()
  assert.equal(frames.size, 0)
  viewport.dispatchEvent(new Event('resize'))
  viewport.dispatchEvent(new Event('scroll'))
  browser.dispatchEvent(new Event('resize'))
  flush()
  assert.equal(frames.size, 0)
  assert.equal(style.value.height, '780px')
})
