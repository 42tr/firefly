<script setup>
import UiIcon from './UiIcon.vue'
import CopyButton from './CopyButton.vue'
import { filePreviewType, readPreviewBlob } from '../composables/filePreview.js'
import { htmlPreviewErrorMessage, htmlPreviewTooLarge, htmlPreviewUrl, isHtmlPreview } from '../utils/htmlPreview.js'
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'
import { formatAppDateTime } from '../composables/appTimezone.js'

const props = defineProps({ open: Boolean, conversation: Object })
const emit = defineEmits(['close'])
const dialog = ref(null)
const path = ref('')
const entries = ref([])
const search = ref('')
const loading = ref(false)
const error = ref('')
const notice = ref('')
const checking = ref(false)
const previewItem = ref(null)
const previewLoading = ref(false)
const previewError = ref('')
const previewText = ref('')
const previewUrl = ref('')
let previewController

const htmlDialog = ref(null)
const htmlPreviewItem = ref(null)
const htmlPreviewSrc = ref('')
const htmlPreviewLoading = ref(false)
const htmlPreviewError = ref('')
let htmlPreviewController
let htmlLoadTimer
function closePreview() {
  previewController?.abort()
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value)
  previewUrl.value = ''
  previewText.value = ''
  previewItem.value = null
  previewError.value = ''
  previewLoading.value = false
}
async function preview(item) {
  closePreview()
  const type = filePreviewType(item)
  if (!type) return
  previewItem.value = item
  if (item.size > type.limit) {
    previewError.value = `文件超过预览上限（${type.limit / 1024 / 1024}MB），请下载查看`
    return
  }
  const request = new AbortController()
  previewController = request
  previewLoading.value = true
  try {
    const response = await apiFetch(`${endpoint('/download')}?${new URLSearchParams({ path: item.path, directory: 'false' })}`, { credentials: 'include', signal: request.signal })
    if (!response.ok) throw new Error(await errorMessage(response))
    const blob = await readPreviewBlob(response, type)
    if (request.signal.aborted) return
    if (type.kind === 'image') previewUrl.value = URL.createObjectURL(blob)
    else {
      const text = await blob.text()
      if (request.signal.aborted) return
      if (text.includes('\0')) throw new Error('文件包含二进制内容，请下载查看')
      previewText.value = text
    }
  } catch (err) {
    if (!request.signal.aborted) previewError.value = err.message || '预览失败，请重试'
  } finally {
    if (!request.signal.aborted) previewLoading.value = false
  }
}
function closeHtmlPreview() {
  htmlPreviewController?.abort()
  htmlPreviewController = undefined
  clearTimeout(htmlLoadTimer)
  htmlLoadTimer = undefined
  // 先清空 src 移除 iframe（停止其中的脚本与网络活动），再关对话框。
  htmlPreviewSrc.value = ''
  htmlDialog.value?.close()
  htmlPreviewItem.value = null
  htmlPreviewError.value = ''
  htmlPreviewLoading.value = false
}

async function openHtmlPreview(item) {
  if (!item) return
  closePreview()
  closeHtmlPreview()
  htmlPreviewItem.value = item
  htmlPreviewError.value = ''
  htmlPreviewLoading.value = true
  await nextTick()
  htmlDialog.value?.showModal()
  const request = new AbortController()
  htmlPreviewController = request
  const url = htmlPreviewUrl(endpoint('/preview'), item.path)
  try {
    // 错误响应带全局 X-Frame-Options: DENY，先 HEAD 预检才能在抽屉里显示中文提示。
    const response = await apiFetch(url, { method: 'HEAD', credentials: 'include', signal: request.signal })
    if (!response.ok) throw new Error(htmlPreviewErrorMessage(response.status))
    if (request.signal.aborted) return
    htmlPreviewSrc.value = url
    htmlLoadTimer = setTimeout(() => {
      if (htmlPreviewLoading.value) {
        htmlPreviewLoading.value = false
        htmlPreviewError.value = '页面加载超时，请下载查看'
      }
    }, 15000)
  } catch (err) {
    if (!request.signal.aborted) {
      htmlPreviewLoading.value = false
      htmlPreviewError.value = err.message || '预览失败，请重试'
    }
  }
}

function onHtmlFrameLoad() {
  clearTimeout(htmlLoadTimer)
  htmlLoadTimer = undefined
  htmlPreviewLoading.value = false
  // 超时之后才到的 load 说明页面其实已渲染，错误条（position:absolute）必须一起收起。
  htmlPreviewError.value = ''
}

/** 超限提示；正常条目返回空串，由模板回落到常规 aria-label。 */
function htmlPreviewSizeHint(item) {
  return htmlPreviewTooLarge(item.size) ? '超过 8MB，请下载查看' : ''
}

let controller
let downloadController
const frames = new Set()
const visibleEntries = computed(() => entries.value.filter(item => item.name.toLocaleLowerCase().includes(search.value.toLocaleLowerCase())))
const crumbs = computed(() => path.value.split('/').filter(Boolean).map((name, index, parts) => ({ name, path: parts.slice(0, index + 1).join('/') })))
const endpoint = (suffix = '') => `/api/assistant/conversations/${props.conversation.id}/files${suffix}`
const errorMessage = async response => {
  const data = await response.json().catch(() => ({}))
  return data.error || (response.status === 401 ? '登录已过期，请重新登录' : '文件操作失败，请重试')
}

async function load(target = path.value) {
  closePreview()
  closeHtmlPreview()
  controller?.abort()
  const request = new AbortController()
  controller = request
  loading.value = true
  error.value = ''
  try {
    const response = await apiFetch(`${endpoint()}?${new URLSearchParams({ path: target })}`, { credentials: 'include', signal: request.signal })
    if (!response.ok) throw new Error(await errorMessage(response))
    const data = await response.json()
    if (request.signal.aborted) return
    path.value = data.path
    entries.value = data.entries
    search.value = ''
  } catch (err) {
    if (!request.signal.aborted) error.value = err.message
  } finally {
    if (!request.signal.aborted) loading.value = false
  }
}

async function download(item) {
  downloadController?.abort()
  const request = new AbortController()
  downloadController = request
  const conversationId = props.conversation.id
  const url = `${endpoint('/download')}?${new URLSearchParams({ path: item.path, directory: String(item.is_dir) })}`
  checking.value = true
  error.value = ''
  notice.value = ''
  try {
    const response = await apiFetch(url, { method: 'HEAD', credentials: 'include', signal: request.signal })
    if (!response.ok) {
      // HEAD has no response body; listing provides a useful retry route.
      throw new Error(response.status === 401 ? '登录已过期，请重新登录' : '文件不可访问，请刷新目录后重试')
    }
    if (request.signal.aborted) return
    // Native downloads stream to disk, including large builds. Same-origin error
    // responses load in the hidden frame and can be shown without leaving chat.
    const frame = document.createElement('iframe')
    frame.hidden = true
    frame.title = '文件下载'
    frames.add(frame)
    frame.onload = () => {
      let text
      try { text = frame.contentDocument?.body?.textContent } catch { text = '下载失败或登录已过期' }
      if (!text) return
      if (props.open && props.conversation?.id === conversationId) {
        try {
          const data = JSON.parse(text)
          error.value = data.error || '下载失败，请重试'
        } catch { error.value = '下载失败或登录已过期，请重试' }
        notice.value = ''
      }
      frame.remove()
      frames.delete(frame)
    }
    frame.src = url
    document.body.appendChild(frame)
    notice.value = item.is_dir ? '已请求打包下载，准备完成后浏览器会开始下载。大目录可能需要几分钟。' : '已请求下载，请查看浏览器下载列表。'
  } catch (err) {
    if (!request.signal.aborted) error.value = err.message
  } finally {
    if (!request.signal.aborted) checking.value = false
  }
}

function size(value) {
  if (value == null) return '文件夹'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let index = 0
  while (value >= 1024 && index < units.length - 1) { value /= 1024; index++ }
  return `${index ? value.toFixed(1) : value} ${units[index]}`
}
const modified = value => value == null ? '—' : formatAppDateTime(value * 1000)

watch([() => props.open, () => props.conversation?.id], async ([open, conversationId]) => {
  controller?.abort()
  downloadController?.abort()
  closePreview()
  closeHtmlPreview()
  checking.value = false
  if (!open) {
    // Native dialog.close() restores focus only when the panel was open.
    dialog.value?.close()
    return
  }
  path.value = ''
  entries.value = []
  search.value = ''
  notice.value = ''
  await nextTick()
  if (!props.open || props.conversation?.id !== conversationId || !dialog.value) return
  if (!dialog.value.open) {
    dialog.value.showModal()
  }
  load('')
})

onBeforeUnmount(() => {
  controller?.abort()
  downloadController?.abort()
  closePreview()
  closeHtmlPreview()
  frames.forEach(frame => frame.remove())
})
</script>

<template>
  <dialog ref="dialog" class="files-drawer" aria-labelledby="files-title" @cancel.prevent="htmlPreviewItem ? closeHtmlPreview() : emit('close')" @click="event => { if (event.target === dialog) emit('close') }">
    <section class="files-content">
      <header>
        <div><h2 id="files-title">文件</h2><p class="workspace">{{ conversation?.cwd || 'workspace' }}</p></div>
        <button type="button" class="button ghost small" aria-label="关闭文件面板" autofocus @click="emit('close')">关闭</button>
      </header>
      <nav aria-label="文件路径">
        <button type="button" :disabled="loading" @click="load('')">工作目录</button>
        <template v-for="crumb in crumbs" :key="crumb.path"><span>/</span><button type="button" :disabled="loading" @click="load(crumb.path)">{{ crumb.name }}</button></template>
      </nav>
      <div class="file-actions">
        <input v-model="search" type="search" placeholder="按名称搜索当前目录" aria-label="按名称搜索当前目录">
        <button type="button" class="button ghost small" :disabled="loading" @click="load()">刷新</button>
        <button type="button" class="button ghost small" :disabled="loading || checking" @click="download({ path, is_dir: true })">打包当前目录</button>
      </div>
      <p v-if="error" class="file-error" role="alert">{{ error }} <button type="button" @click="load()">重试</button></p>
      <p v-if="notice" class="file-notice" role="status">{{ notice }}</p>
      <section v-if="previewItem" class="file-preview" aria-label="文件预览" :aria-busy="previewLoading">
        <div class="preview-toolbar">
          <button type="button" class="button ghost small" @click="closePreview">返回列表</button>
          <strong>{{ previewItem.name }}</strong>
          <CopyButton v-if="!previewLoading && !previewError && !previewUrl && previewText" :text="previewText" label="复制内容" />
          <button type="button" class="button ghost small" :disabled="checking" @click="download(previewItem)">下载</button>
        </div>
        <p v-if="previewLoading" class="empty" role="status">正在读取预览…</p>
        <p v-else-if="previewError" class="file-error" role="alert">{{ previewError }} <button type="button" @click="preview(previewItem)">重试</button></p>
        <div v-else class="preview-body">
          <img v-if="previewUrl" :src="previewUrl" :alt="previewItem.name" @error="previewError = '图片无法预览，请下载查看'">
          <pre v-else-if="previewText" tabindex="0" aria-label="文件文本内容">{{ previewText }}</pre>
          <p v-else class="empty">文件内容为空</p>
        </div>
      </section>
      <div v-else class="file-list" :aria-busy="loading">
        <p v-if="loading" class="empty" role="status">正在读取文件…</p>
        <template v-else>
          <p v-if="!visibleEntries.length" class="empty">{{ search ? '没有匹配的文件' : (error ? '暂时无法读取目录' : '当前目录为空') }}</p>
          <div v-for="item in visibleEntries" :key="item.path" class="file-row">
            <UiIcon class="file-icon" :name="item.is_dir ? 'folder' : 'file'" />
            <div class="file-info">
              <button v-if="item.is_dir" type="button" class="file-name folder" @click="load(item.path)">{{ item.name }}</button>
              <button v-else-if="isHtmlPreview(item)" type="button" class="file-name folder" @click="openHtmlPreview(item)">{{ item.name }}</button>
              <button v-else-if="filePreviewType(item)" type="button" class="file-name folder" :aria-label="`预览 ${item.name}`" @click="preview(item)">{{ item.name }}</button>
              <span v-else class="file-name">{{ item.name }}</span>
              <div class="file-meta">{{ size(item.size) }} · {{ modified(item.modified_at) }}</div>
            </div>
            <button v-if="isHtmlPreview(item)" type="button" class="button ghost small" :disabled="htmlPreviewTooLarge(item.size)" :title="htmlPreviewSizeHint(item) || undefined" :aria-label="htmlPreviewSizeHint(item) || `在应用内预览 ${item.name}`" @click="openHtmlPreview(item)">预览</button>
            <button type="button" class="button ghost small" :disabled="checking" :aria-label="`${item.is_dir ? '打包下载' : '下载'} ${item.name}`" @click="download(item)">{{ item.is_dir ? '打包下载' : '下载' }}</button>
          </div>
        </template>
      </div>
      <footer>点击文本或图片文件名可预览（文本 ≤ 1MB，图片 ≤ 10MB）。HTML 原型可在应用内沙箱预览（≤ 8MB），仅支持自包含的内联脚本/样式与 data: 图片。目录下载为 ZIP，不包含符号链接。</footer>
    </section>
  </dialog>

  <dialog
    ref="htmlDialog"
    class="html-preview"
    aria-labelledby="html-preview-title"
    @cancel.prevent="closeHtmlPreview"
    @click="event => { if (event.target === htmlDialog) closeHtmlPreview() }"
  >
    <header>
      <h2 id="html-preview-title">{{ htmlPreviewItem?.name }}</h2>
      <div class="html-preview-actions">
        <button type="button" class="button ghost small" :disabled="checking" @click="download(htmlPreviewItem)">下载</button>
        <button type="button" class="button ghost small" aria-label="关闭 HTML 预览" @click="closeHtmlPreview">关闭</button>
      </div>
    </header>
    <p v-if="htmlPreviewLoading" class="empty html-preview-status" role="status">正在打开预览…</p>
    <p v-if="htmlPreviewError" class="file-error html-preview-status" role="alert">
      {{ htmlPreviewError }}
      <button type="button" @click="openHtmlPreview(htmlPreviewItem)">重试</button>
    </p>
    <iframe
      v-if="htmlPreviewSrc"
      :src="htmlPreviewSrc"
      :title="`预览 ${htmlPreviewItem?.name || ''}`"
      sandbox="allow-scripts allow-forms allow-downloads"
      referrerpolicy="no-referrer"
      @load="onHtmlFrameLoad"
    ></iframe>
  </dialog>
</template>

<style scoped>
.file-preview { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 12px; }
.preview-toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.preview-toolbar strong { flex: 1; min-width: 80px; overflow-wrap: anywhere; font-size: 13px; }
.preview-body { overflow: auto; min-height: 0; flex: 1; }
.preview-body pre { margin: 0; padding: 12px; background: var(--bg-subtle); font: 12px/1.6 var(--font-mono); tab-size: 4; }
.preview-body img { display: block; max-width: 100%; height: auto; margin: auto; }

.files-drawer { position: fixed; inset: 0 0 0 auto; margin: 0; padding: 0; width: min(560px, 100vw); height: 100dvh; max-width: 100vw; max-height: 100dvh; border: 0; border-left: 1px solid var(--border-soft); color: var(--text-primary); background: var(--bg-panel); box-shadow: -12px 0 40px #0002; }
.files-drawer::backdrop { background: #0005; }
.files-content { display: flex; flex-direction: column; height: 100%; padding: 20px; box-sizing: border-box; gap: 16px; }
header { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
h2 { margin: 0; font-size: 20px; }
.workspace { font-family: var(--font-mono); margin: 6px 0 0; overflow-wrap: anywhere; color: var(--text-secondary); font-size: 12px; }
nav { display: flex; flex-wrap: wrap; gap: 6px; font-size: 13px; }
nav button, .folder, .file-error button { border: 0; padding: 0; background: none; color: var(--accent); cursor: pointer; font: inherit; text-align: left; overflow-wrap: anywhere; }
.file-actions { display: flex; flex-wrap: wrap; gap: 8px; }
input { min-width: 120px; flex: 1; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); padding: 8px 10px; background: var(--bg-body); color: inherit; }
.file-list { flex: 1; min-height: 0; overflow: auto; }
.file-row { display: flex; align-items: center; gap: 10px; padding: 14px 0; border-bottom: 1px solid var(--border-soft); }
.file-info { flex: 1; min-width: 0; }
.file-name { display: block; font-size: 14px; overflow-wrap: anywhere; }
.file-meta { font-size: 12px; color: var(--text-secondary); margin-top: 5px; }
.file-icon { width: 22px; height: 22px; color: var(--muted); }
.file-row > button { flex-shrink: 0; }
.empty { text-align: center; color: var(--text-secondary); padding: 36px 0; }
.file-error, .file-notice { margin: 0; font-size: 13px; overflow-wrap: anywhere; }
.file-error { color: #c44545; }
.file-notice, footer { color: var(--text-secondary); }
footer { font-size: 12px; line-height: 1.6; }
.html-preview { width: min(1100px, calc(100vw - 24px)); height: min(850px, 90dvh); max-width: none; max-height: none; padding: 0; border: 1px solid var(--border-soft); border-radius: 14px; background: var(--bg-panel); color: var(--text-primary); }
.html-preview::backdrop { background: rgba(15, 23, 42, .45); }
.html-preview > header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 16px; border-bottom: 1px solid var(--border-soft); }
.html-preview h2 { font-size: 16px; overflow-wrap: anywhere; }
.html-preview-actions { display: flex; gap: 8px; flex-shrink: 0; }
.html-preview-status { position: absolute; top: 57px; left: 0; right: 0; margin: 0; padding: 12px 16px; text-align: left; background: var(--bg-panel); }
.html-preview iframe { display: block; width: 100%; height: calc(100% - 57px); border: 0; background: #fff; }
@media (max-width: 640px) { .files-drawer { width: 100vw; border: 0; } .files-content { padding: 16px; padding-bottom: max(16px, env(safe-area-inset-bottom)); } }
</style>
