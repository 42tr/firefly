// The sandbox has an opaque origin. Bind every message to its exact WindowProxy;
// never accept a frame-supplied module ID, endpoint or host credential.
export function createModuleBridge({ frameWindow, moduleId, request, send, input, result, context, close = () => {}, clipboard = null, openLink = null }) {
  let closed = false, initialized = false, inFlight = 0, handlesClose = false
  const notify = (method, params) => send({ jsonrpc: '2.0', method, params })
  const receive = async event => {
    if (closed || event.source !== frameWindow || event.origin !== 'null') return
    const message = event.data
    if (!message || message.jsonrpc !== '2.0' || typeof message.method !== 'string') return
    const hasId = typeof message.id === 'string' || typeof message.id === 'number'
    const respond = payload => { if (!closed && hasId) send({ jsonrpc: '2.0', id: message.id, ...payload }) }
    if (message.method === 'ui/notifications/initialized') {
      if (initialized) {
        notify('ui/notifications/tool-input', { arguments: input })
        notify('ui/notifications/tool-result', result)
      }
      return
    }
    if (!hasId) return
    try {
      if (message.method === 'ui/initialize') {
        initialized = true
        handlesClose = message.params?.appCapabilities?.closeRequests === true
        respond({ result: { protocolVersion: '2026-01-26', hostInfo: { name: 'Firefly', version: '0.1.0' }, hostCapabilities: { serverTools: {} }, hostContext: context } })
      } else if (message.method === 'ping') respond({ result: {} })
      else if (message.method === 'ui/close' && initialized) { respond({ result: {} }); close() }
      else if (message.method === 'ui/clipboard' && initialized && clipboard) {
        const params = message.params || {}
        const text = typeof params.text === 'string' && params.text.length <= 1024 * 1024 && params.png === undefined
        const png = typeof params.png === 'string' && params.png.startsWith('data:image/png;base64,') && params.png.length <= 12 * 1024 * 1024 && params.text === undefined
        if (!text && !png) throw new Error('复制内容无效')
        await clipboard(text ? { text: params.text } : { png: params.png }); respond({ result: {} })
      } else if (message.method === 'ui/open-link' && initialized && openLink) {
        const url = new URL(message.params?.url)
        if (!['https:', 'http:'].includes(url.protocol)) throw new Error('链接协议无效')
        openLink(url.href); respond({ result: {} })
      } else if (message.method === 'tools/call' && initialized) {
        if (inFlight >= 4) throw new Error('页面请求过多，请稍后重试')
        const { name, arguments: args = {} } = message.params || {}
        if (typeof name !== 'string' || !args || typeof args !== 'object' || Array.isArray(args)) throw new Error('工具参数无效')
        inFlight++
        try { respond({ result: await request(moduleId, name, args) }) }
        finally { inFlight-- }
      } else respond({ error: { code: -32601, message: '此宿主尚未支持该能力' } })
    } catch (error) { respond({ error: { code: -32000, message: error.message || '模块调用失败' } }) }
  }
  return { receive, requestClose() { if (initialized && handlesClose) notify('ui/notifications/close-request', {}); else close() }, close() { closed = true } }
}
