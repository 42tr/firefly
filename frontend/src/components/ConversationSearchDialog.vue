<script setup>
import { nextTick, onBeforeUnmount, ref, watch } from 'vue'
import UiIcon from './UiIcon.vue'
import ConversationUnreadBadge from './ConversationUnreadBadge.vue'
import { conversationAgentLabel, conversationDirectoryLabel } from '../utils/conversationLabels.js'

const props = defineProps({
  open: Boolean,
  conversations: { type: Array, default: () => [] },
  loading: Boolean,
  error: { type: String, default: '' },
  hasMore: Boolean,
  formatTitle: { type: Function, required: true },
  formatWorkspace: { type: Function, default: () => '' },
})
const emit = defineEmits(['close', 'select', 'refresh', 'show-more'])
const search = defineModel('search', { type: String, default: '' })
const dialog = ref(null)
const input = ref(null)
const results = ref(null)
let previousFocus = null
let previousOverflow

const releaseDialog = () => {
  if (dialog.value?.open) dialog.value.close()
  if (previousOverflow !== undefined) {
    document.body.style.overflow = previousOverflow
    previousOverflow = undefined
  }
  if (previousFocus?.isConnected) previousFocus.focus()
  previousFocus = null
}
watch(() => props.open, async open => {
  if (!open) return releaseDialog()
  previousFocus = document.activeElement
  await nextTick()
  if (!props.open || !dialog.value) return
  previousOverflow = document.body.style.overflow
  document.body.style.overflow = 'hidden'
  dialog.value.showModal()
  input.value?.focus()
}, { immediate: true })
onBeforeUnmount(releaseDialog)

const close = () => emit('close')
const handleDialogKeydown = event => {
  if (event.key !== 'Escape' || event.isComposing || event.keyCode === 229) return
  event.preventDefault()
  event.stopPropagation()
  close()
}
const select = id => {
  if (!props.loading && !props.error) emit('select', id)
}
const handleInputKeydown = event => {
  if (event.isComposing || event.keyCode === 229 || props.loading || props.error) return
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    results.value?.querySelector('.search-result')?.focus()
  } else if (event.key === 'Enter' && props.conversations.length) {
    event.preventDefault()
    select(props.conversations[0].id)
  }
}
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" id="conversation-search" class="conversation-search-dialog" aria-labelledby="conversation-search-title"
      @cancel.prevent="close" @close="open && close()" @keydown="handleDialogKeydown" @click.self="close">
      <header class="search-dialog-header">
        <h2 id="conversation-search-title">搜索对话</h2>
        <button class="search-close" type="button" aria-label="关闭搜索" title="关闭搜索" @click="close">×</button>
      </header>
      <div class="search-field">
        <UiIcon name="search" />
        <input ref="input" v-model="search" type="search" autofocus autocomplete="off" placeholder="搜索标题、目录或 Agent"
          aria-label="搜索标题、目录或 Agent" aria-controls="conversation-search-results" @keydown="handleInputKeydown">
      </div>
      <div ref="results" id="conversation-search-results" class="search-results" :aria-busy="loading">
        <p v-if="loading" class="search-state" role="status">搜索中…</p>
        <div v-else-if="error" class="search-state" role="alert">
          <p>暂时无法加载对话</p><button class="button ghost small" type="button" @click="emit('refresh')">重试</button>
        </div>
        <template v-else>
          <p class="search-results-label">{{ search.trim() ? '搜索结果' : '最近对话' }}</p>
          <p v-if="!conversations.length" class="search-state" role="status">{{ search.trim() ? '没有匹配的对话' : '暂无对话' }}</p>
          <button v-for="conversation in conversations" :key="conversation.id" class="search-result" type="button" @click="select(conversation.id)">
            <UiIcon name="chat" />
            <span class="search-result-copy"><span class="search-result-title">{{ formatTitle(conversation) }}</span>
              <span class="search-result-meta" :title="formatWorkspace(conversation) || conversation.cwd">{{ [conversationDirectoryLabel(conversation), conversationAgentLabel(conversation)].filter(Boolean).join(' · ') }}</span>
            </span>
            <ConversationUnreadBadge v-if="conversation.hasUnread && conversation.status === 'completed'" />
          </button>
          <button v-if="hasMore" class="search-more" type="button" @click="emit('show-more')">加载更多</button>
        </template>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.conversation-search-dialog { width: min(620px, calc(100vw - 32px)); max-width: none; height: min(560px, calc(100dvh - 48px)); max-height: min(680px, calc(100dvh - 48px)); padding: 0; border: 1px solid var(--border-soft); border-radius: 24px; background: var(--bg-panel); color: var(--text-primary); box-shadow: var(--shadow-dialog); overflow: hidden; }
.conversation-search-dialog[open] { display: flex; flex-direction: column; animation: popIn 180ms var(--ease-out); }
.conversation-search-dialog::backdrop { background: rgba(15, 20, 34, 0.32); }
.search-dialog-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 16px 20px 4px; flex-shrink: 0; }
.search-dialog-header h2 { margin: 0; font-size: 18px; font-weight: 600; }
.search-close { display: grid; place-items: center; width: 44px; height: 44px; border: 0; border-radius: 10px; background: transparent; color: var(--muted); font: inherit; font-size: 26px; cursor: pointer; }
.search-close:hover, .search-result:hover, .search-more:hover { background: var(--bg-hover); }
.search-field { display: flex; align-items: center; gap: 12px; flex-shrink: 0; margin: 8px 20px 16px; padding: 0 14px; border: 1px solid var(--border-soft); border-radius: 14px; }
.search-field:focus-within { border-color: var(--focus-ring); }
.search-field > .ui-icon { width: 20px; height: 20px; flex-shrink: 0; color: var(--muted); }
.search-field input { width: 100%; min-width: 0; min-height: 48px; padding: 8px 0; border: 0; outline: none; background: transparent; color: var(--text-primary); font: inherit; font-size: 15px; }
.search-results { flex: 1; min-height: 120px; overflow-y: auto; overscroll-behavior: contain; padding: 8px 14px 14px; border-top: 1px solid var(--border-soft); }
.search-results-label { margin: 4px 10px 8px; color: var(--muted); font-size: 12px; }
.search-result { display: flex; align-items: center; gap: 12px; width: 100%; min-height: 58px; padding: 10px; border: 0; border-radius: 10px; background: transparent; color: var(--text-primary); font: inherit; text-align: left; cursor: pointer; }
.search-result > .ui-icon { width: 20px; height: 20px; flex-shrink: 0; }
.search-result-copy { display: flex; flex: 1; flex-direction: column; gap: 4px; min-width: 0; }
.search-result-title { font-size: 15px; overflow-wrap: anywhere; }
.search-result-meta { color: var(--muted); font-size: 12px; overflow-wrap: anywhere; }
.search-state { padding: 24px 10px; color: var(--muted); font-size: 14px; text-align: center; }
.search-more { width: 100%; min-height: 44px; border: 0; border-radius: 10px; background: transparent; color: var(--muted); font: inherit; font-size: 13px; cursor: pointer; }
:where(button):focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
@media (max-width: 520px) { .conversation-search-dialog { width: calc(100vw - 20px); height: min(560px, calc(100dvh - 24px)); max-height: calc(100dvh - 24px); border-radius: 20px; } .search-dialog-header { padding: 12px 16px 4px; } .search-field { margin: 8px 16px 12px; } }
@media (prefers-reduced-motion: reduce) { .conversation-search-dialog[open] { animation: none; } }
</style>
