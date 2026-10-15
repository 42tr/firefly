import { rpc } from '../bridge.js'
// The host accepts four simultaneous app tool calls. Reserve slots for queued
// requests so pages with several independent data sources can load reliably.
let active = 0
const waiting = []
async function callTool(params) {
 if (active < 4) active++
 else await new Promise(resolve => waiting.push(resolve))
 try { return await rpc('tools/call', params) }
 finally { const next = waiting.shift(); if (next) next(); else active-- }
}
export async function apiFetch(path,options={}) {
 if(options.signal?.aborted)throw new DOMException('Aborted','AbortError')
 const method=(options.method||'GET').toUpperCase()
 const args=method==='GET'?{path}:{path,method,...(options.body?{body:JSON.parse(options.body)}:{})}
 const result=await callTool({name:method==='GET'?'read':'request',arguments:args})
 if(options.signal?.aborted)throw new DOMException('Aborted','AbortError')
 if(result.isError)throw new Error(result.content?.find(c=>c.type==='text')?.text||'模块调用失败')
 const value=result.structuredContent
 if(!Number.isInteger(value?.status))throw new Error('模块返回了无效响应')
 const body=value.base64===undefined?JSON.stringify(value.body):Uint8Array.from(atob(value.base64),c=>c.charCodeAt(0))
 return new Response([204,205,304].includes(value.status)?null:body,{status:value.status,headers:value.headers})
}
export function redirectToLogin(){throw new Error('登录已失效，请重新打开主页面')}
