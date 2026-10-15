<script setup>
import { onBeforeUnmount, ref, watch, nextTick } from 'vue'
import { moduleRequest, jsonRequest, useModules } from '../composables/useModules'
import { createModuleBridge } from '../composables/moduleBridge'
import { getAppTimezone } from '../composables/appTimezone.js'

const props = defineProps({ module: Object, page: Object })
const emit = defineEmits(['close'])
const { invalidateIndicators } = useModules()
const close = () => { void invalidateIndicators(); emit('close') }
const dialog = ref(null), frame = ref(null), source = ref(''), loading = ref(true), error = ref('')
let bridge, revision = 0, opened, readinessTimer
const onMessage = event => bridge?.receive(event)
const requestClose = () => bridge ? bridge.requestClose() : emit('close')
window.addEventListener('message', onMessage)
const attachBridge = () => {
  bridge?.close()
  if (!frame.value?.contentWindow || !opened) return
  const frameWindow = frame.value.contentWindow
  bridge = createModuleBridge({
    frameWindow, moduleId: props.module.id, input: opened.arguments, result: opened.result,
    close,
    clipboard: async ({ text, png }) => {
      if (png) { const data = Uint8Array.from(atob(png.split(',')[1]), c => c.charCodeAt(0)); await navigator.clipboard.write([new ClipboardItem({ 'image/png': new Blob([data], { type: 'image/png' }) })]) }
      else await navigator.clipboard.writeText(text)
    },
    openLink: url => window.open(url, '_blank', 'noopener,noreferrer'),
    context: { origin: window.location.origin, theme: 'light', locale: navigator.language, timeZone: getAppTimezone(), displayMode: 'inline', containerDimensions: { width: frame.value.clientWidth, height: frame.value.clientHeight } },
    send: message => { frameWindow.postMessage(message, '*'); if (message.result?.hostInfo) { loading.value = false; clearTimeout(readinessTimer) } },
    request: (id, name, args) => moduleRequest(`/${encodeURIComponent(id)}/call`, jsonRequest('POST', { name, arguments: args })),
  })
}
watch(() => [props.module?.id, props.page?.id], async () => {
  const current = ++revision
  bridge?.close(); source.value = ''; error.value = ''; loading.value = true
  await nextTick(); dialog.value?.showModal()
  try {
    opened = await moduleRequest(`/${encodeURIComponent(props.module.id)}/pages/${encodeURIComponent(props.page.id)}/open`, jsonRequest('POST', {}))
    if (revision !== current) return
    // Attach before navigation so the iframe's first initialize request isn't lost.
    attachBridge()
    source.value = `/api/modules/${encodeURIComponent(props.module.id)}/pages/${encodeURIComponent(props.page.id)}/html`
    readinessTimer = setTimeout(() => { if (loading.value) { loading.value = false; error.value = '模块页面未完成初始化，请检查模块日志。' } }, 15000)
  } catch (cause) { if (revision === current) { error.value = cause.message; loading.value = false } }
}, { immediate: true })
onBeforeUnmount(() => { revision++; bridge?.close(); clearTimeout(readinessTimer); window.removeEventListener('message', onMessage); dialog.value?.close() })
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="module-app" :aria-label="page.title" @cancel.prevent="requestClose" @click.self="requestClose">
      <header><h2>{{ page.title }}</h2><button class="button ghost" aria-label="关闭模块页面" @click="requestClose">关闭</button></header>
      <p v-if="loading" role="status">正在打开…</p><p v-if="error" role="alert">{{ error }}</p>
      <iframe ref="frame" :src="source || 'about:blank'" :title="page.title" sandbox="allow-scripts allow-forms allow-downloads" referrerpolicy="no-referrer"></iframe>
    </dialog>
  </Teleport>
</template>

<style scoped>
.module-app { width: min(1000px, calc(100vw - 24px)); max-width: none; height: min(850px, 90dvh); padding: 0; border: 1px solid var(--border-soft); border-radius: 14px; background: var(--bg-panel); color: var(--text-primary); }
.module-app::backdrop { background: rgba(15,23,42,.35); }
header { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; border-bottom: 1px solid var(--border-soft); }
h2 { font-size: 17px; margin: 0; } p { margin: 12px 16px; } iframe { width: 100%; height: calc(100% - 68px); border: 0; background: white; }
</style>
