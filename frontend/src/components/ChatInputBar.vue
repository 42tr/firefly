<script setup>
import UiIcon from './UiIcon.vue'
import { isVideoAttachment, isTextAttachment } from '../composables/attachmentTypes.js'
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from 'vue'
import { apiFetch } from '@/utils/http'
import { useWorkspaceCompletion } from '../composables/useWorkspaceCompletion.js'

const props = defineProps({
  attachments: {
    type: Array,
    default: () => [],
  },
  attachmentError: {
    type: String,
    default: '',
  },
  isStreaming: {
    type: Boolean,
    default: false,
  },
  hasQueuedMessages: Boolean,
  sendDisabled: Boolean,
  attachmentsEnabled: { type: Boolean, default: true },
  actionLabel: { type: String, default: '' },
  inputLabel: { type: String, default: '消息' },
  inputPlaceholder: { type: String, default: '' },
  submitOnEnter: { type: Boolean, default: true },
  disabled: {
    type: Boolean,
    default: false,
  },
  disabledMessage: {
    type: String,
    default: '',
  },
  goalStatus: { type: String, default: 'idle' },
  goalEnabled: { type: Boolean, default: false },
})

const emit = defineEmits(['send', 'stop', 'remove-attachment', 'attachment-change', 'attachment-paste', 'toggle-goal'])

const model = defineModel({ type: String, default: '' })

const inputEl = ref(null)
const directoryListId = useId()
const {
  open: directoryOpen, entries: directories, loading: directoriesLoading,
  error: directoryError, selected: selectedDirectory, sync: syncDirectories,
  close: closeDirectories, choose: chooseDirectory, keydown: directoryKeydown,
} = useWorkspaceCompletion({
  model, inputEl,
  disabled: computed(() => props.disabled),
  fetchWorkspace: async (path, signal) => {
    const params = new URLSearchParams({ path })
    const response = await apiFetch(`/api/assistant/workspace?${params}`, { signal })
    if (!response.ok) throw new Error('读取 workspace 失败，请重新输入 /// 重试')
    return response.json()
  },
})
watch(selectedDirectory, async () => {
  await nextTick()
  document.getElementById(`${directoryListId}-${selectedDirectory.value}`)?.scrollIntoView({ block: 'nearest' })
})
const fileInput = ref(null)
const previewDialog = ref(null)
const previewAttachment = ref(null)
const previewImageUrl = ref('')
const previewText = ref('')
const previewLoading = ref(false)
const previewError = ref('')
let previewRequest = 0
const queueing = computed(() => props.isStreaming || props.hasQueuedMessages)
// 跟随移动端尺寸调整输入框，保留清晰的输入提示。
const mobileMedia = window.matchMedia('(max-width: 768px)')
const isMobile = ref(mobileMedia.matches)
const handleMediaChange = (event) => {
  isMobile.value = event.matches
}
mobileMedia.addEventListener('change', handleMediaChange)
onBeforeUnmount(() => mobileMedia.removeEventListener('change', handleMediaChange))
onBeforeUnmount(() => previewDialog.value?.close())
const placeholder = computed(() => {
  if (props.inputPlaceholder) return props.inputPlaceholder
  if (isMobile.value) return ''
  if (props.disabled) return props.disabledMessage
  if (queueing.value) return '输入下一条消息，将按队列顺序处理'
  return '输入消息…'
})

const handleKeydown = (event) => {
  if (props.disabled) return
  // 中文输入法组词期间的 Enter 是确认候选词，不应触发发送
  if (event.isComposing || event.keyCode === 229) return
  if (directoryKeydown(event)) return
  if (event.key === 'Enter' && !event.shiftKey && (props.submitOnEnter || event.ctrlKey || event.metaKey)) {
    event.preventDefault()
    if (!props.sendDisabled && (model.value.trim() || props.attachments.length)) emit('send')
  }
}

const handleInput = (e) => {
  const input = e.target
  const box = input.closest?.('.input-box')
  const previousMinHeight = box?.style.minHeight
  // Keep the composer footprint while measuring. Otherwise the temporary
  // `auto` height clamps the chat panel's scrollTop before the new height is
  // applied, which makes history jump on every wrapped-line keystroke.
  if (box) box.style.minHeight = `${box.offsetHeight}px`
  input.style.height = 'auto'
  input.style.height = `${input.scrollHeight}px`
  if (box) box.style.minHeight = previousMinHeight
}

const canPreviewAttachment = (attachment) => Boolean(attachment?.previewUrl || isTextAttachment(attachment))

const closePreview = () => {
  previewRequest += 1
  previewDialog.value?.close()
  previewAttachment.value = null
  previewImageUrl.value = ''
  previewText.value = ''
  previewError.value = ''
  previewLoading.value = false
}

const previewAttachmentFile = async (attachment) => {
  if (!canPreviewAttachment(attachment)) return
  const request = ++previewRequest
  previewAttachment.value = attachment
  previewImageUrl.value = attachment.previewUrl || ''
  previewText.value = ''
  previewError.value = ''
  previewLoading.value = false

  if (!previewImageUrl.value) {
    if (attachment.size > 1024 * 1024) {
      previewError.value = '文件超过预览上限（1MB），请下载查看'
    } else if (!attachment.file?.text) {
      previewError.value = '无法读取文件内容，请下载查看'
    } else {
      previewLoading.value = true
      try {
        const text = await attachment.file.text()
        if (request !== previewRequest) return
        previewText.value = text
      } catch (error) {
        if (request !== previewRequest) return
        previewError.value = error?.message || '预览失败，请重试'
      } finally {
        if (request === previewRequest) previewLoading.value = false
      }
    }
  }
  if (request === previewRequest && !previewDialog.value?.open) previewDialog.value?.showModal()
}

watch(
  () => props.attachments.map((attachment) => attachment.id),
  (ids) => {
    if (previewAttachment.value && !ids.includes(previewAttachment.value.id)) closePreview()
  },
)

// 草稿恢复、快捷短语及清空输入也需要同步高度。
watch([model, inputEl, isMobile], async () => {
  await nextTick()
  if (inputEl.value) handleInput({ target: inputEl.value })
})
</script>

<template>
  <div class="input-bar">
    <div class="input-container">
      <slot name="controls"></slot>
      <div v-if="attachments.length" class="attachment-preview-bar">
        <div
          v-for="attachment in attachments"
          :key="attachment.id"
          class="attachment-preview"
        >
          <button
            v-if="canPreviewAttachment(attachment)"
            class="attachment-preview-trigger"
            type="button"
            :aria-label="`预览 ${attachment.name}`"
            @click="previewAttachmentFile(attachment)"
          >
            <img
              v-if="attachment.previewUrl && !isVideoAttachment(attachment)"
              :src="attachment.previewUrl"
              :alt="attachment.name"
              class="attachment-preview-thumb"
            />
            <UiIcon v-else class="attachment-preview-icon" :name="isVideoAttachment(attachment) ? 'video' : 'attachment'" />
            <span class="attachment-preview-name">{{ attachment.name }}</span>
          </button>
          <template v-else>
            <UiIcon class="attachment-preview-icon" name="attachment" />
            <span class="attachment-preview-name">{{ attachment.name }}</span>
          </template>
          <button
            class="attachment-preview-remove"
            type="button"
            @click="emit('remove-attachment', attachment.id)"
            aria-label="移除附件"
          >
            ×
          </button>
        </div>
      </div>
      <div v-if="attachmentError" class="attachment-error">{{ attachmentError }}</div>
      <div v-if="directoryOpen" class="directory-completion" @mousedown.prevent>
        <div class="directory-completion-heading">workspace 目录 <span>↑↓ 选择 · Tab 进入子目录 · Enter 确认 · Esc 关闭</span></div>
        <p v-if="directoriesLoading" role="status">正在读取目录…</p>
        <p v-else-if="directoryError" role="alert">{{ directoryError }}</p>
        <p v-else-if="!directories.length" role="status">没有匹配的目录</p>
        <div :id="directoryListId" role="listbox" aria-label="workspace 目录" class="directory-completion-list">
          <button
            v-for="(directory, index) in directories"
            :id="`${directoryListId}-${index}`"
            :key="directory.path"
            type="button"
            role="option"
            tabindex="-1"
            :aria-selected="selectedDirectory === index"
            :class="{ selected: selectedDirectory === index }"
            @click="chooseDirectory(directory)"
          ><UiIcon name="folder" /><span>{{ directory.name }}</span></button>
        </div>
      </div>
      <div class="input-box" :class="{ disabled: disabled }">
        <button v-if="attachmentsEnabled" class="attachment-button" data-icon-motion="lift" type="button" title="添加附件" aria-label="添加附件" :disabled="disabled" @click="fileInput?.click()">
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 3v10M3 8h10" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" /></svg>
        </button>
          <input
            v-if="attachmentsEnabled"
            ref="fileInput"
            type="file"
            multiple
            :disabled="disabled"
            @change="emit('attachment-change', $event)"
            hidden
          />

          <textarea
            :aria-label="inputLabel"
            ref="inputEl"
            v-model="model"
            class="chat-input"
            rows="1"
            :disabled="disabled"
            aria-autocomplete="list"
            :aria-controls="directoryOpen ? directoryListId : undefined"
            :aria-activedescendant="directoryOpen && directories.length ? `${directoryListId}-${selectedDirectory}` : undefined"
          @keydown="handleKeydown"
          @input="handleInput($event); !$event.isComposing && syncDirectories()"
          @compositionend="syncDirectories"
          @click="syncDirectories"
          @keyup="['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes($event.key) && syncDirectories()"
          @blur="closeDirectories"
          @paste="emit('attachment-paste', $event)"
          :placeholder="placeholder"
        ></textarea>
        <button
          class="goal-mode-toggle compact"
          :class="{ enabled: goalEnabled, paused: goalStatus === 'paused' && !goalEnabled }"
          type="button"
          :aria-pressed="goalEnabled"
          :title="goalEnabled ? '关闭 Goal：本条消息按普通对话发送' : goalStatus === 'paused' ? '继续之前的 Goal，并将本条消息作为目标消息发送' : '仅本条消息启用 Goal 模式'"
          :disabled="disabled"
          @click="emit('toggle-goal')"
        >
          <UiIcon name="target" />
          <span>Goal</span>
          <span v-if="goalEnabled" class="goal-mode-dot" aria-hidden="true"></span>
        </button>
        <button
          v-if="isStreaming"
          class="stop-button"
          type="button"
          title="停止当前回复，排队消息仍会继续处理"
          aria-label="停止当前回复"
          @click="emit('stop')"
        ><svg viewBox="0 0 16 16" aria-hidden="true"><rect x="4.5" y="4.5" width="7" height="7" rx="1.2" fill="currentColor" /></svg></button>
        <button
          class="send-button"
          :class="{ queued: queueing, labeled: actionLabel }"
          type="button"
          :title="actionLabel || (queueing ? '加入队列' : '发送消息')"
          :aria-label="actionLabel || (queueing ? '加入队列' : '发送消息')"
          :disabled="disabled || sendDisabled || (!model.trim() && !attachments.length)"
          @click="emit('send')"
        ><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 11.5V4.5m0 0L5.2 7.3M8 4.5l2.8 2.8" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" /></svg><span v-if="actionLabel || queueing">{{ actionLabel || '排队' }}</span></button>
      </div>
    </div>
  </div>
  <Teleport to="body">
    <dialog
      ref="previewDialog"
      class="attachment-preview-dialog"
      :aria-label="previewAttachment ? `预览 ${previewAttachment.name}` : '附件预览'"
      @click.self="closePreview"
      @close="closePreview"
    >
      <div class="attachment-preview-dialog-toolbar">
        <strong>{{ previewAttachment?.name }}</strong>
        <button type="button" class="attachment-preview-dialog-close" aria-label="关闭预览" @click="closePreview">×</button>
      </div>
      <div class="attachment-preview-dialog-body" :aria-busy="previewLoading">
        <p v-if="previewLoading" class="attachment-preview-dialog-status">正在读取预览…</p>
        <p v-else-if="previewError" class="attachment-preview-dialog-error" role="alert">{{ previewError }}</p>
        <video v-else-if="previewImageUrl && isVideoAttachment(previewAttachment)" :src="previewImageUrl" :aria-label="previewAttachment?.name" controls playsinline preload="metadata"
          @error="previewError = '浏览器无法播放此视频，仍可上传原文件'" />
        <img v-else-if="previewImageUrl" :src="previewImageUrl" :alt="previewAttachment?.name || ''" />
        <pre v-else-if="previewText" tabindex="0">{{ previewText }}</pre>
        <p v-else class="attachment-preview-dialog-status">文件内容为空</p>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.input-bar {
  padding: 8px var(--chat-gutter) 28px;
  background: transparent;
  flex-shrink: 0;
}

.input-container {
  position: relative;
  max-width: var(--chat-content-width);
  margin: 0 auto;
}

.directory-completion {
  position: absolute;
  bottom: calc(100% + 8px);
  left: 0;
  right: 0;
  z-index: 20;
  padding: 6px;
  border: 1px solid var(--border-glass);
  border-radius: var(--radius);
  background: var(--bg-glass-strong);
  backdrop-filter: blur(18px);
  -webkit-backdrop-filter: blur(18px);
  box-shadow: var(--shadow-float);
  animation: popIn 0.18s var(--ease-out);
}
.directory-completion-heading { display: flex; flex-wrap: wrap; gap: 4px 12px; padding: 6px 8px; font-size: 12px; color: var(--muted); }
.directory-completion-heading span { margin-left: auto; }
.directory-completion p { margin: 8px; font-size: 13px; color: var(--muted); }
.directory-completion-list { max-height: min(240px, 35dvh); overflow-y: auto; }
.directory-completion-list button { display: flex; align-items: center; gap: 8px; width: 100%; padding: 10px; border: 0; border-radius: var(--radius-sm); transition: background 0.15s ease; background: transparent; color: var(--text-primary); font: inherit; text-align: left; cursor: pointer; }
.directory-completion-list button.selected,
.directory-completion-list button:hover { background: var(--glow-soft); }
.directory-completion-list button span { overflow-wrap: anywhere; min-width: 0; }
.directory-completion-list .ui-icon { flex-shrink: 0; }

.attachment-preview-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 8px;
}

.attachment-preview {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 10px 4px 4px;
  background: var(--bg-glass-strong);
  border-radius: 999px;
  border: 1px solid var(--border-glass);
  box-shadow: var(--shadow-xs);
  font-size: 12px;
  animation: popIn 0.25s var(--ease-spring) both;
}

.attachment-preview-trigger {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  padding: 0;
  border: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  cursor: zoom-in;
}

.attachment-preview-trigger:hover .attachment-preview-name {
  color: var(--accent);
}

.attachment-preview-thumb {
  width: 24px;
  height: 24px;
  object-fit: cover;
  border-radius: 999px;
}

.attachment-preview-name {
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.attachment-preview-remove {
  background: transparent;
  border: none;
  cursor: pointer;
  color: var(--muted);
  font-size: 15px;
  line-height: 1;
  padding: 2px;
  border-radius: 50%;
}

.attachment-preview-remove:hover {
  color: var(--danger);
}

.attachment-preview-dialog {
  position: fixed;
  inset: 0;
  box-sizing: border-box;
  width: 100%;
  height: 100%;
  max-width: none;
  max-height: none;
  margin: 0;
  padding: 64px 24px 24px;
  border: 0;
  background: transparent;
}

.attachment-preview-dialog[open] {
  display: flex;
  flex-direction: column;
}

.attachment-preview-dialog::backdrop {
  background: rgb(10 14 24 / 72%);
}

.attachment-preview-dialog-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  width: min(900px, 100%);
  margin: 0 auto 12px;
  color: #fff;
}

.attachment-preview-dialog-toolbar strong {
  min-width: 0;
  flex: 1;
  overflow-wrap: anywhere;
}

.attachment-preview-dialog-close {
  width: 40px;
  height: 40px;
  flex: 0 0 auto;
  border: 1px solid rgb(255 255 255 / 40%);
  border-radius: 50%;
  background: #222;
  color: #fff;
  font-size: 28px;
  line-height: 1;
  cursor: pointer;
}

.attachment-preview-dialog-body {
  width: min(900px, 100%);
  min-height: 0;
  flex: 1;
  margin: 0 auto;
  overflow: auto;
  border-radius: var(--radius-sm);
}

.attachment-preview-dialog-body img,
.attachment-preview-dialog-body video {
  display: block;
  max-width: 100%;
  max-height: 100%;
  margin: auto;
  object-fit: contain;
}

.attachment-preview-dialog-body pre {
  box-sizing: border-box;
  min-height: 100%;
  margin: 0;
  padding: 16px;
  overflow: auto;
  background: var(--bg-panel);
  color: var(--text-primary);
  font: 13px/1.6 var(--font-mono);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.attachment-preview-dialog-status,
.attachment-preview-dialog-error {
  margin: 0;
  padding: 20px;
  background: var(--bg-panel);
  color: var(--text-primary);
}

.attachment-preview-dialog-error {
  color: var(--danger);
}

.attachment-error {
  color: var(--danger);
  font-size: 12px;
  margin-bottom: 8px;
}

.goal-mode-toggle { display: inline-flex; align-items: center; gap: 6px; min-height: 32px; padding: 0 9px; border: 1px solid var(--border-soft); border-radius: 999px; background: var(--bg-panel); color: var(--muted); font: inherit; font-size: 12px; cursor: pointer; transition: background .18s ease, border-color .18s ease, color .18s ease, box-shadow .18s ease; }
.goal-mode-toggle .ui-icon { width: 14px; height: 14px; }
.goal-mode-toggle:hover:not(:disabled) { border-color: #f1c26c; color: #8a5a0b; background: #fffaf0; }
.goal-mode-toggle.enabled { border-color: #f1c26c; background: #fff5dd; color: #99630a; box-shadow: 0 0 0 2px rgba(246,169,59,.1); font-weight: 650; }
.goal-mode-toggle.paused { border-color: #cfd9e1; color: var(--accent); }
.goal-mode-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--glow); box-shadow: 0 0 0 3px var(--glow-soft); }

.input-box {
  position: relative;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 10px 5px 12px;
  min-height: 52px;
  box-sizing: border-box;
  background: var(--bg-panel);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  box-shadow: 0 2px 8px rgba(23, 32, 51, 0.05);
  transition: border-color 0.2s ease, box-shadow 0.2s ease;
}

@media (min-width: 769px) {
  .input-box:focus-within {
    border-color: #c6a66e;
    box-shadow: 0 0 0 2px var(--focus-soft);
  }
}

.input-box.disabled {
  opacity: 0.75;
}

.attachment-button {
  width: 36px;
  height: 36px;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  background: transparent;
  border: none;
  border-radius: 50%;
  cursor: pointer;
  color: var(--muted);
  font-size: 18px;
  transition: color 0.2s var(--ease-out), transform 0.35s var(--ease-spring);
}

.attachment-button:hover:not(:disabled) {
  color: #8a5a0b;
}

.chat-input {
  flex: 1;
  min-width: 0;
  resize: none;
  border: none;
  outline: none;
  background: transparent;
  padding: 6px 2px;
  font-family: inherit;
  /* iOS Safari 会对小于 16px 的输入控件自动缩放页面 */
  font-size: 16px !important;
  line-height: 1.5;
  color: var(--text-primary);
  min-height: 36px;
  max-height: 160px;
  overflow-y: auto;
  scrollbar-width: none;
  -ms-overflow-style: none;
}

.chat-input::-webkit-scrollbar {
  display: none;
}

.chat-input::placeholder {
  color: var(--muted);
}

.send-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  height: 36px;
  width: 36px;
  padding: 0;
  border: none;
  border-radius: 50%;
  background: var(--glow);
  color: var(--ink);
  font-family: inherit;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.2s ease, transform 0.2s ease;
}

.send-button.labeled,
.send-button.queued { width: auto; height: 32px; padding: 0 10px; gap: 4px; border-radius: 16px; font-size: 12px; line-height: 1; }

.send-button svg,
.stop-button svg,
.attachment-button svg {
  display: block;
  width: 16px;
  height: 16px;
}

.send-button:hover:not(:disabled) {
  background: #edaa34;
}

.send-button:active:not(:disabled) {
  transform: scale(0.92);
}

.send-button:disabled {
  background: var(--bg-hover);
  color: var(--muted);
  box-shadow: none;
  cursor: not-allowed;
}

.stop-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  width: 32px;
  height: 32px;
  padding: 0;
  border: none;
  border-radius: 50%;
  background: #fff1f1;
  color: var(--danger);
  cursor: pointer;
  font-size: 12px;
  box-shadow: 0 0 0 1px rgba(220, 38, 38, 0.2);
  transition: background 0.2s ease, transform 0.25s var(--ease-spring);
}

.stop-button:hover {
  background: #ffe2e2;
  transform: scale(1.06);
}

@media (max-width: 768px) {
  .input-bar {
    padding: 8px var(--chat-gutter) calc(10px + var(--composer-bottom-inset, env(safe-area-inset-bottom)));
  }

  .input-box {
    min-height: 56px;
    padding: 5px 6px;
    border-radius: 16px;
  }
}
@media (pointer: coarse) {
  .attachment-button, .send-button, .stop-button, .attachment-preview-remove { width: 44px; height: 44px; }
  .send-button.labeled, .send-button.queued { width: auto; height: 44px; }
}
</style>
