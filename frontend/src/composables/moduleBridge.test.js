import test from 'node:test'
import assert from 'node:assert/strict'
import { createModuleBridge } from './moduleBridge.js'

function fixture(request = async () => ({ content: [] })) {
  const frame = {}, messages = [], calls = []
  const bridge = createModuleBridge({ frameWindow: frame, moduleId: 'trusted-module', request: (...args) => { calls.push(args); return request(...args) }, send: message => messages.push(message), input: {}, result: { content: [] }, context: { theme: 'light' } })
  const receive = (method, params, extra = {}) => bridge.receive({ source: frame, origin: 'null', data: { jsonrpc: '2.0', id: 1, method, params }, ...extra })
  return { bridge, messages, calls, frame, receive }
}
test('only the bound opaque frame can initialize or invoke its own module', async () => {
  const f = fixture()
  await f.receive('ui/initialize', {}, { source: {} })
  await f.receive('ui/initialize', {}, { origin: 'https://other.test' })
  assert.equal(f.messages.length, 0)
  await f.receive('tools/call', { name: 'write', arguments: {} })
  assert.equal(f.calls.length, 0)
  await f.receive('ui/initialize', {})
  await f.receive('tools/call', { name: 'read', arguments: {}, moduleId: 'victim', url: '/auth' })
  assert.deepEqual(f.calls, [['trusted-module', 'read', {}]])
  await f.receive('ui/message', { content: [] })
  assert.equal(f.messages.at(-1).error.code, -32601)
})
test('closed frames cannot call tools or receive delayed responses', async () => {
  let resolve
  const f = fixture(() => new Promise(done => { resolve = done }))
  await f.receive('ui/initialize', {})
  const pending = f.receive('tools/call', { name: 'slow', arguments: {} })
  f.bridge.close()
  resolve({ content: [] })
  await pending
  assert.equal(f.messages.length, 1)
  await f.receive('tools/call', { name: 'another', arguments: {} })
  assert.equal(f.calls.length, 1)
})
test('tool inputs/results are delivered after initialization and malformed calls rejected', async () => {
  const f = fixture()
  await f.receive('ui/notifications/initialized')
  assert.equal(f.messages.length, 0)
  await f.receive('ui/initialize', {})
  await f.receive('ui/notifications/initialized')
  assert.equal(f.messages[1].method, 'ui/notifications/tool-input')
  assert.equal(f.messages[2].method, 'ui/notifications/tool-result')
  await f.receive('tools/call', { name: 'read', arguments: [] })
  assert.equal(f.calls.length, 0)
  assert.equal(f.messages.at(-1).error.code, -32000)
})

test('close requests let a module confirm unsaved work before closing', async () => {
  const frame={},messages=[];let closed=0
  const bridge=createModuleBridge({frameWindow:frame,moduleId:'owned',request:async()=>({}),send:m=>messages.push(m),context:{},close:()=>closed++})
  const receive=(method,params)=>bridge.receive({source:frame,origin:'null',data:{jsonrpc:'2.0',id:1,method,params}})
  await receive('ui/initialize',{appCapabilities:{closeRequests:true}})
  bridge.requestClose()
  assert.equal(closed,0)
  assert.equal(messages.at(-1).method,'ui/notifications/close-request')
  await receive('ui/close',{})
  assert.equal(closed,1)
})

test('host link handling rejects executable URLs and ignores other frames', async () => {
  const frame={},opened=[],messages=[]
  const bridge=createModuleBridge({frameWindow:frame,moduleId:'owned',request:async()=>({}),send:m=>messages.push(m),context:{},openLink:url=>opened.push(url)})
  const receive=(method,params)=>bridge.receive({source:frame,origin:'null',data:{jsonrpc:'2.0',id:1,method,params}})
  await receive('ui/initialize',{})
  await receive('ui/open-link',{url:'javascript:alert(1)'})
  assert.equal(opened.length,0)
  assert.equal(messages.at(-1).error.code,-32000)
  await receive('ui/open-link',{url:'https://example.com/help'})
  assert.deepEqual(opened,['https://example.com/help'])
})

test('clipboard accepts one bounded format and rejects conflicting payloads', async () => {
  const frame = {}, writes = [], messages = []
  const bridge = createModuleBridge({ frameWindow: frame, moduleId: 'owned', request: async () => ({}), send: m => messages.push(m), context: {}, clipboard: data => writes.push(data) })
  const receive = (method, params) => bridge.receive({ source: frame, origin: 'null', data: { jsonrpc: '2.0', id: 1, method, params } })
  await receive('ui/initialize', {})
  for (const params of [{ text: 'ok', png: 'invalid' }, { png: 'data:text/html;base64,AAAA' }, { text: 'x'.repeat(1024 * 1024 + 1) }]) {
    await receive('ui/clipboard', params)
    assert.equal(messages.at(-1).error.code, -32000)
  }
  assert.deepEqual(writes, [])
  await receive('ui/clipboard', { text: '内容', ignored: true })
  await receive('ui/clipboard', { png: 'data:image/png;base64,AAAA' })
  assert.deepEqual(writes, [{ text: '内容' }, { png: 'data:image/png;base64,AAAA' }])
})
