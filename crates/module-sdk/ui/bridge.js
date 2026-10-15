let sequence=0, initialized=false
const pending=new Map()
export let hostContext={}
export function rpc(method,params={}) {
 return new Promise((resolve,reject)=>{
  const id=++sequence
  const timer=setTimeout(()=>{pending.delete(id);reject(new Error('模块调用超时'))},950000)
  pending.set(id,{resolve,reject,timer});window.parent.postMessage({jsonrpc:'2.0',id,method,params},'*')
 })
}
let requestClose=()=>{}
export function onClose(callback){requestClose=callback}
export function closeApp(){return rpc('ui/close')}
globalThis.window?.addEventListener('message',event=>{
 if(event.source!==window.parent || event.data?.jsonrpc!=='2.0') return
 const message=event.data
 if(message.method==='ui/notifications/close-request'){requestClose();return}
 const item=pending.get(message.id);if(!item)return
 pending.delete(message.id);clearTimeout(item.timer)
 message.error?item.reject(new Error(message.error.message)):item.resolve(message.result)
})
export async function initialize(){
 if(initialized)return
 const result=await rpc('ui/initialize',{protocolVersion:'2026-01-26',appInfo:{name:'Firefly module',version:'1.0.0'},appCapabilities:{closeRequests:true}})
 hostContext=result.hostContext;initialized=true
 window.parent.postMessage({jsonrpc:'2.0',method:'ui/notifications/initialized'},'*')
 // External links are opened by the host after URL validation; no popup sandbox escape.
 document.addEventListener('click',event=>{
  const link=event.target.closest?.('a[href]');if(!link || link.hasAttribute('download'))return
  const href=link.getAttribute('href')
  if(/^https?:\/\//i.test(href)){event.preventDefault();rpc('ui/open-link',{url:href}).catch(()=>{})}
 })
}
export function confirmAction(message){
 return new Promise(resolve=>{
  const dialog=document.createElement('dialog');dialog.className='module-confirm'
  const text=document.createElement('p');text.textContent=message
  const footer=document.createElement('footer')
  const finish=value=>{dialog.close();dialog.remove();resolve(value)}
  for(const [label,value] of [['取消',false],['确定',true]]){
   const button=document.createElement('button');button.className='button';button.textContent=label;button.onclick=()=>finish(value);footer.append(button)
  }
  dialog.append(text,footer);dialog.addEventListener('cancel',event=>{event.preventDefault();finish(false)})
  document.body.append(dialog);dialog.showModal();footer.firstChild.focus()
 })
}
