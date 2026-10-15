<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import UiIcon from './UiIcon.vue'
import ConversationUnreadBadge from './ConversationUnreadBadge.vue'

const props = defineProps({
  conversations: { type: Array, default: () => [] },
  activeConversationId: { type: [Number, String], default: null },
  active: { type: Boolean, default: true },
  loading: Boolean,
  error: { type: String, default: '' },
  formatTitle: { type: Function, required: true },
})
const emit = defineEmits(['select', 'refresh'])
const root = ref(null), trigger = ref(null), menu = ref(null), open = ref(false)
const menuStyle = ref({})
const recent = computed(() => props.conversations
  .filter(item => !item.is_archived && item.message_count > 0)
  .sort((a, b) => (Date.parse(b.updated_at || b.created_at) || 0) - (Date.parse(a.updated_at || a.created_at) || 0)
    || Number(b.id) - Number(a.id))
  .slice(0, 10))
const items = () => [...(menu.value?.querySelectorAll('[role="menuitem"]') || [])]
const close = (restoreFocus = false) => {
  open.value = false
  if (restoreFocus) trigger.value?.focus()
}
const reposition = async () => {
  if (!open.value || !trigger.value) return
  const rect = trigger.value.getBoundingClientRect()
  const width = window.innerWidth || 1024, height = window.innerHeight || 768
  const menuWidth = Math.min(320, width - 24)
  const left = Math.max(12, Math.min(rect.right - 4, width - menuWidth - 12))
  menuStyle.value = { left: `${left}px`, top: '12px', width: `${menuWidth}px`, maxHeight: `${height - 24}px` }
  await nextTick()
  if (!open.value) return
  // Use the layout height so an entrance transform cannot push the popup off-screen.
  const menuHeight = menu.value?.offsetHeight || 0
  menuStyle.value.top = `${Math.max(12, Math.min(rect.top - 12, height - menuHeight - 12))}px`
}
const show = async (last = false) => {
  if (!props.active) return
  open.value = true
  emit('refresh')
  await nextTick()
  await reposition()
  if (open.value) ((last ? items().at(-1) : items()[0]) || menu.value)?.focus()
}
const select = conversation => {
  close(true)
  emit('select', conversation.id)
}
const refresh = () => {
  if (props.loading) return
  // The error button disappears when loading starts; keep focus in the popup.
  menu.value?.focus()
  emit('refresh')
}
const navigate = event => {
  if (event.key === 'Escape') {
    event.preventDefault()
    event.stopPropagation()
    close(true)
  } else if (event.key === 'Tab') close(true)
  else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    event.preventDefault()
    const buttons = items()
    if (!buttons.length) return
    const index = buttons.indexOf(document.activeElement)
    const target = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1
      : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length
    buttons[target]?.focus()
  }
}
const contains = target => root.value?.contains(target) || menu.value?.contains(target)
const outside = event => { if (!contains(event.target)) close() }
const focusOut = event => { if (event.relatedTarget && !contains(event.relatedTarget)) close() }
watch(() => props.active, active => { if (!active) close() })
watch([recent, () => props.loading, () => props.error], reposition, { flush: 'post' })
onMounted(() => {
  document.addEventListener('pointerdown', outside)
  window.addEventListener('resize', reposition)
  window.addEventListener('scroll', reposition, true)
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', outside)
  window.removeEventListener('resize', reposition)
  window.removeEventListener('scroll', reposition, true)
})
</script>

<template>
  <div ref="root" class="recent-chats" @focusout="focusOut">
    <button ref="trigger" class="recent-chats-toggle" type="button" title="最近聊天" aria-label="查看最近聊天"
      aria-haspopup="menu" :aria-expanded="open" aria-controls="recent-chats-menu"
      @click="open ? close() : show()" @keydown.down.prevent="show()" @keydown.up.prevent="show(true)">
      <UiIcon name="chat" />
    </button>
    <Teleport to="body">
      <Transition name="recent-chats">
        <section v-if="open" id="recent-chats-menu" ref="menu" class="recent-chats-menu" :style="menuStyle"
          role="menu" aria-label="最近聊天" tabindex="-1" @keydown="navigate" @focusout="focusOut">
          <h2 class="recent-chats-heading">最近聊天</h2>
          <div class="recent-chats-list">
            <button v-for="conversation in recent" :key="conversation.id" class="recent-chat-item" type="button"
              role="menuitem" :title="formatTitle(conversation)" :aria-current="conversation.id === activeConversationId ? 'page' : undefined"
              @click="select(conversation)">
              <span class="recent-chat-title">{{ formatTitle(conversation) }}</span>
              <ConversationUnreadBadge v-if="conversation.hasUnread && conversation.status === 'completed'" />
            </button>
            <p v-if="loading && !recent.length" class="recent-chats-state" role="status">正在加载…</p>
            <p v-else-if="!error && !recent.length" class="recent-chats-state" role="status">暂无最近聊天</p>
          </div>
          <div v-if="error" class="recent-chats-state recent-chats-error" role="status">
            <span>暂时无法加载最近聊天</span>
            <button type="button" role="menuitem" class="recent-chats-retry" :disabled="loading" @click="refresh">重试</button>
          </div>
        </section>
      </Transition>
    </Teleport>
  </div>
</template>

<style scoped>
.recent-chats { width: 44px; flex-shrink: 0; }
.recent-chats-toggle { display: flex; align-items: center; justify-content: center; width: 44px; height: 44px; padding: 6px; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--text-secondary); cursor: pointer; }
.recent-chats-toggle .ui-icon { width: 22px; height: 22px; }
.recent-chats-toggle:hover, .recent-chats-toggle[aria-expanded='true'] { background: var(--bg-hover); color: var(--text-primary); }
.recent-chats-menu { position: fixed; z-index: 110; display: flex; flex-direction: column; box-sizing: border-box; padding: 12px; border: 1px solid var(--border-soft); border-radius: 24px; background: var(--bg-panel); box-shadow: 0 6px 28px rgba(23, 32, 51, .08); transform-origin: top left; animation: none; }
.recent-chats-heading { flex-shrink: 0; margin: 0; padding: 10px 12px 12px; color: var(--muted); font: inherit; font-size: 14px; font-weight: 600; line-height: 22px; }
.recent-chats-list { min-height: 0; overflow-y: auto; overscroll-behavior: contain; }
.recent-chat-item { display: flex; align-items: center; gap: 8px; width: 100%; min-height: 44px; padding: 11px 12px; border: 0; border-radius: 10px; background: transparent; color: var(--text-primary); font: inherit; font-size: 15px; line-height: 22px; text-align: left; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; cursor: pointer; }
.recent-chat-item:hover, .recent-chat-item[aria-current='page'] { background: var(--bg-hover); }
.recent-chat-title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.recent-chats-state { margin: 0; padding: 12px; color: var(--muted); font-size: 13px; line-height: 1.6; }
.recent-chats-error { flex-shrink: 0; }
.recent-chats-retry { min-height: 44px; margin-left: 8px; padding: 4px 8px; border: 0; border-radius: var(--radius-sm); background: var(--bg-hover); color: var(--text-primary); font: inherit; cursor: pointer; }
.recent-chats-enter-active, .recent-chats-leave-active { transition: opacity 160ms var(--ease-out), transform 160ms var(--ease-out); }
.recent-chats-enter-from, .recent-chats-leave-to { opacity: 0; transform: translateX(-4px); }
</style>
