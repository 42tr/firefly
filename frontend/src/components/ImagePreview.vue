<script setup>
import { onBeforeUnmount, ref } from 'vue'
import { readPreviewBlob } from '../composables/filePreview.js'
import { apiFetch } from '@/utils/http'
import UiIcon from './UiIcon.vue'

const dialog = ref(null)
const preview = ref(null)
const loading = ref(false)
const error = ref('')
let requestId = 0
let controller

const open = (src, alt = '') => {
  if (!src) return
  controller?.abort()
  requestId += 1
  loading.value = false
  error.value = ''
  preview.value = { kind: 'image', src, alt }
  if (!dialog.value?.open) dialog.value?.showModal()
}

const openText = async (src, name = '') => {
  if (!src) return
  controller?.abort()
  const request = new AbortController()
  controller = request
  const currentRequest = ++requestId
  loading.value = true
  error.value = ''
  preview.value = { kind: 'text', name, text: '' }
  if (!dialog.value?.open) dialog.value?.showModal()
  try {
    const response = await apiFetch(src, { credentials: 'include', signal: request.signal })
    if (!response.ok) throw new Error(`文件读取失败（${response.status}）`)
    const blob = await readPreviewBlob(response, { mime: 'text/plain', limit: 1024 * 1024 })
    const text = await blob.text()
    if (currentRequest !== requestId) return
    preview.value = { kind: 'text', name, text }
  } catch (err) {
    if (currentRequest !== requestId) return
    error.value = err.message || '文件无法预览，请下载查看'
  } finally {
    if (currentRequest === requestId) loading.value = false
  }
}

const openVideo = (src, name = '') => {
  if (!src) return
  open(src, name)
  preview.value = { kind: 'video', src, name }
}

const close = () => {
  controller?.abort()
  requestId += 1
  dialog.value?.close()
  preview.value = null
  loading.value = false
  error.value = ''
}

onBeforeUnmount(close)

defineExpose({ open, openText, openVideo })
</script>

<template>
  <Teleport to="body">
    <dialog
      ref="dialog"
      class="image-preview"
      :aria-label="preview?.kind === 'image' ? '图片预览' : `文件预览：${preview?.name || ''}`"
      @click.self="close"
      @close="close"
    >
      <button class="image-preview-close" type="button" aria-label="关闭预览" autofocus @click="close"><UiIcon name="x" /></button>
      <p v-if="loading" class="image-preview-status">正在读取文件…</p>
      <p v-else-if="error" class="image-preview-error" role="alert">
        {{ error }}
        <a v-if="preview?.kind === 'video'" :href="preview.src" :download="preview.name">下载原文件</a>
      </p>
      <video v-else-if="preview?.kind === 'video'" :src="preview.src" :aria-label="preview.name" controls playsinline preload="metadata"
        @error="error = '浏览器无法播放此视频，请下载查看。'" />
      <img v-else-if="preview?.kind === 'image'" :src="preview.src" :alt="preview.alt"
        @error="error = '图片无法加载，文件可能已被清理或暂时无法访问。'" />
      <pre v-else-if="preview?.kind === 'text'" tabindex="0">{{ preview.text || '文件内容为空' }}</pre>
    </dialog>
  </Teleport>
</template>

<style scoped>
.image-preview {
  position: fixed;
  inset: 0;
  box-sizing: border-box;
  width: 100%;
  height: 100%;
  max-width: none;
  max-height: none;
  margin: 0;
  padding: calc(64px + env(safe-area-inset-top, 0px)) 16px calc(24px + env(safe-area-inset-bottom, 0px));
  border: 0;
  background: transparent;
}

.image-preview[open] {
  display: flex;
  align-items: center;
  justify-content: center;
}

.image-preview::backdrop {
  background: rgb(0 0 0 / 80%);
}

.image-preview img,
.image-preview video {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
}

.image-preview pre {
  box-sizing: border-box;
  width: min(960px, 100%);
  max-height: 100%;
  margin: auto;
  padding: 16px;
  overflow: auto;
  border-radius: 8px;
  background: #fff;
  color: #222;
  font: 13px/1.6 var(--font-mono);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  text-align: left;
}

.image-preview-status,
.image-preview-error {
  margin: auto;
  padding: 20px;
  border-radius: 8px;
  background: #fff;
  color: #222;
}

.image-preview-error {
  color: #b42318;
}

.image-preview-close {
  position: absolute;
  top: calc(16px + env(safe-area-inset-top, 0px));
  right: 16px;
  width: 40px;
  height: 40px;
  border: 1px solid rgb(255 255 255 / 40%);
  border-radius: 50%;
  background: #222;
  color: #fff;
  font-size: 28px;
  cursor: pointer;
}
</style>
