import { initialize, closeApp, onClose } from '@firefly-module-sdk/bridge.js'
import { apiFetch } from '@firefly-module-sdk/utils/http.js'
import '@firefly-module-sdk/style.css'
import './style.css'

const element = id => document.getElementById(id)
const buttons = [element('increment'), element('reset'), element('retry')]
let ready = false

async function request(path = '/counter', method = 'GET') {
  buttons.forEach(button => { button.disabled = true })
  element('error').hidden = true
  element('retry').hidden = true
  element('status').textContent = '正在处理…'
  try {
    if (!ready) { await initialize(); ready = true }
    const response = await apiFetch(path, { method })
    const counter = await response.json()
    if (!response.ok) throw new Error(counter.error || '计数器请求失败')
    element('label').textContent = counter.label
    element('value').textContent = counter.value
    element('step').textContent = `每次增加 ${counter.step}`
    element('status').textContent = '已读取最新数值'
  } catch (error) {
    element('error').textContent = error.message || '计数器请求失败'
    element('error').hidden = false
    element('retry').hidden = false
    element('status').textContent = ''
  } finally {
    buttons.forEach(button => { button.disabled = false })
  }
}

element('increment').addEventListener('click', () => request('/increment', 'POST'))
element('reset').addEventListener('click', () => request('/reset', 'POST'))
element('retry').addEventListener('click', () => request())
element('close').addEventListener('click', () => closeApp())
onClose(() => closeApp())
await request()
