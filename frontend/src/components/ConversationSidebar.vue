<script setup>
import UiIcon from './UiIcon.vue'
import RecentChats from './RecentChats.vue'
import SidebarSettingsMenu from './SidebarSettingsMenu.vue'
import ConversationUnreadBadge from './ConversationUnreadBadge.vue'
import ConversationSearchDialog from './ConversationSearchDialog.vue'
import { conversationAgentLabel, conversationCreationDirectoryLabel, conversationCreationDirectoryPath } from '../utils/conversationLabels.js'
import ModuleIndicator from './ModuleIndicator.vue'
import { useModules } from '../composables/useModules.js'
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'

const { activeIndicators: moduleIndicators, indicatorDescription: moduleDescription } = useModules()
const props = defineProps({
  collapsed: Boolean,
  showingArchived: Boolean,
  archivingConversationIds: { type: Array, default: () => [] },
  recentConversations: { type: Array, default: () => [] },
  pinningConversationIds: { type: Array, default: () => [] },
  open: {
    type: Boolean,
    default: false,
  },
  conversations: {
    type: Array,
    default: () => [],
  },
  activeConversationId: {
    type: [Number, String],
    default: null,
  },
  isLoading: {
    type: Boolean,
    default: false,
  },
  error: {
    type: String,
    default: '',
  },
  deleteConfirmId: {
    type: [Number, String],
    default: null,
  },
  hasMore: {
    type: Boolean,
    default: false,
  },
  formatConversationTitle: {
    type: Function,
    required: true,
  },
  formatConversationWorkspace: {
    type: Function,
    default: () => '',
  },
  formatConversationMeta: {
    type: Function,
    required: true,
  },

})


const emit = defineEmits([
  'logout',
  'toggle-collapse',
  'close',
  'refresh-recent',
  'refresh-search',
  'select-recent',
  'select',
  'delete',
  'toggle-delete',
  'dismiss-delete',
  'show-more',
  'toggle-archived',
  'rename',
  'toggle-pin',
  'toggle-archive',
  'rename-start',
  'rename-end',
  'open-default',
])

const pinnedConversationCount = computed(() => props.conversations.filter(conversation =>
  conversation.is_pinned && !conversation.is_archived).length)
const conversationGroups = computed(() => [
  { id: 'active', conversations: props.conversations.filter(conversation => !conversation.is_archived) },
  { id: 'archived', conversations: props.conversations.filter(conversation => conversation.is_archived) },
])

const panel = ref(null)
const mobileMedia = window.matchMedia?.('(max-width: 768px)')
const isMobile = ref(mobileMedia?.matches ?? false)
const updateMobile = event => { isMobile.value = event.matches }
onMounted(() => mobileMedia?.addEventListener('change', updateMobile))
onBeforeUnmount(() => mobileMedia?.removeEventListener('change', updateMobile))
const expandButton = ref(null)
const toggleCollapsed = async () => {
  emit('toggle-collapse')
  await nextTick()
  if (props.collapsed) expandButton.value?.focus()
  else panel.value?.querySelector('.brand')?.focus()
}

const editingConversationId = ref(null)
const search = defineModel('search', { type: String, default: '' })
const searchOpen = ref(false)
const closeSearch = () => {
  searchOpen.value = false
  search.value = ''
}
watch(() => props.open, open => {
  if (isMobile.value && !open) closeSearch()
})
watch(isMobile, () => closeSearch())
const openSearch = () => { searchOpen.value = true }
const searchShortcutLabel = /Mac|iPhone|iPad/.test(window.navigator?.platform || '') ? '⌘ K' : 'Ctrl K'
const handleSearchShortcut = event => {
  if (isMobile.value || event.defaultPrevented || event.isComposing || event.keyCode === 229 || event.altKey || event.shiftKey
    || !(event.metaKey || event.ctrlKey) || event.key?.toLowerCase() !== 'k') return
  if (event.target?.closest?.('input, textarea, select, [contenteditable]:not([contenteditable="false"]), .cm-editor')
    || document.querySelector('dialog[open]')) return
  event.preventDefault()
  openSearch()
}
onMounted(() => document.addEventListener('keydown', handleSearchShortcut))
onBeforeUnmount(() => document.removeEventListener('keydown', handleSearchShortcut))
const selectSearchResult = id => {
  closeSearch()
  emit('select-recent', id)
}
const editingConversationTitle = ref('')
const editingTitleInput = ref(null)
const captureEditingTitleInput = (element, conversationId) => {
  if (element && editingConversationId.value === conversationId) editingTitleInput.value = element
}
const assistantIconUrl = `${import.meta.env.BASE_URL}assistant-icon.svg`
const conversationDetail = conversation => [
  props.formatConversationMeta(conversation),
  conversation.codex_model,
  conversation.codex_provider,
  conversation.claude_model,
  conversation.cwd,
  conversation.source_cwd,
].filter(Boolean).join(' · ')

// 每条会话的「…」操作菜单：同一时刻只展开一个，点击菜单外任意处关闭
const menuOpenId = ref(null)
const conversationList = ref(null)
const showMore = async () => {
  const list = conversationList.value
  const scrollTop = list?.scrollTop
  emit('show-more')
  await nextTick()
  if (list && list === conversationList.value) list.scrollTop = scrollTop
}
watch(search, async () => {
  dismissMenu()
  cancelEditingConversation()
  await nextTick()
  if (conversationList.value) conversationList.value.scrollTop = 0
})
watch(() => props.showingArchived, () => {
  dismissMenu()
  cancelEditingConversation()
})
// 列表底部空间不足时菜单/确认气泡向上弹出，避免被滚动容器裁剪
const menuDropUp = ref(false)
let menuTrigger = null
const toggleMenu = (conversationId, event) => {
  if (menuOpenId.value === conversationId) {
    menuOpenId.value = null
    return
  }
  const trigger = event?.currentTarget
  menuTrigger = trigger
  const list = trigger?.closest('.conversation-list')
  if (trigger && list) {
    const triggerRect = trigger.getBoundingClientRect()
    const listRect = list.getBoundingClientRect()
    const spaceBelow = listRect.bottom - triggerRect.bottom
    const spaceAbove = triggerRect.top - listRect.top
    menuDropUp.value = spaceBelow < 180 && spaceAbove > spaceBelow
  } else {
    menuDropUp.value = false
  }
  menuOpenId.value = conversationId
  nextTick(() => trigger?.parentElement?.querySelector('[role="menuitem"]')?.focus())
}
const dismissMenu = () => {
  menuOpenId.value = null
}
const handleMenuKeydown = (event) => {
  if (event.key === 'Escape') {
    event.preventDefault()
    dismissMenu()
    menuTrigger?.focus()
  } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    event.preventDefault()
    const items = [...event.currentTarget.querySelectorAll('[role="menuitem"]')]
    const index = items.indexOf(document.activeElement)
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1
      : (index + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length
    items[next]?.focus()
  }
}
watch(menuOpenId, (value) => {
  if (value == null) document.removeEventListener('click', dismissMenu)
  else document.addEventListener('click', dismissMenu)
})
onBeforeUnmount(() => document.removeEventListener('click', dismissMenu))

const onMenuRename = (conversation) => {
  dismissMenu()
  startEditingConversation(conversation)
}
const onMenuAction = (conversationId) => {
  dismissMenu()
  emit('toggle-delete', conversationId)
}
const startEditingConversation = async (conversation) => {
  if (editingConversationId.value == null) emit('rename-start')
  editingConversationId.value = conversation.id
  editingConversationTitle.value = props.formatConversationTitle(conversation)
  await nextTick()
  const field = editingTitleInput.value
  field?.focus()
  field?.select()
}

const cancelEditingConversation = () => {
  const wasEditing = editingConversationId.value != null
  editingConversationId.value = null
  editingConversationTitle.value = ''
  if (wasEditing) emit('rename-end')
}

const saveConversationTitle = (conversation) => {
  if (editingConversationId.value !== conversation.id) return
  const title = editingConversationTitle.value.trim()
  if (!title || title === props.formatConversationTitle(conversation)) {
    cancelEditingConversation()
    return
  }
  cancelEditingConversation()
  emit('rename', { conversation, title })
}

const handleTitleKeydown = (event, conversation) => {
  // 输入法组词期间的 Enter / Escape 交给输入法处理。
  if (event.isComposing || event.keyCode === 229) return
  if (event.key === 'Enter') {
    event.preventDefault()
    saveConversationTitle(conversation)
  } else if (event.key === 'Escape') {
    event.preventDefault()
    cancelEditingConversation()
  }
}
</script>

<template>
  <ConversationSearchDialog :open="searchOpen && !isMobile" v-model:search="search" :conversations="conversations"
    :loading="isLoading" :error="error" :has-more="hasMore"
    :format-title="formatConversationTitle" :format-workspace="formatConversationWorkspace"
    @close="closeSearch" @select="selectSearchResult" @refresh="emit('refresh-search')" @show-more="emit('show-more')" />
  <aside ref="panel" class="tool-panel" :class="{ open, collapsed }" :inert="isMobile && !open" aria-label="工作区导航">
    <nav class="sidebar-rail" aria-label="快捷导航">
      <button class="rail-button rail-brand" type="button" :title="['流萤', moduleDescription].filter(Boolean).join(' · ')" aria-label="进入流萤" :aria-description="[moduleDescription].filter(Boolean).join(' · ') || undefined" @click="emit('open-default')">
        <img :src="assistantIconUrl" alt="" /><ModuleIndicator v-for="indicator in moduleIndicators.slice(0, 1)" :key="indicator.module_id" :indicator="indicator" />

      </button>
      <button ref="expandButton" class="rail-button" type="button" title="展开侧栏" aria-label="展开侧栏" :aria-expanded="!collapsed" @click="toggleCollapsed"><UiIcon name="sidebar" /></button>
      <button class="rail-button" type="button" title="搜索对话" aria-label="搜索对话" aria-haspopup="dialog" :aria-expanded="searchOpen" aria-controls="conversation-search" @click="openSearch"><UiIcon name="search" /></button>
      <RecentChats :active="collapsed && !isMobile" :conversations="recentConversations" :active-conversation-id="activeConversationId"
        :loading="isLoading" :error="error" :format-title="formatConversationTitle"
        @refresh="emit('refresh-recent')" @select="emit('select-recent', $event)" />
    </nav>
    <div class="sidebar-header">
      <button class="brand" type="button" :title="['进入默认对话', moduleDescription].filter(Boolean).join(' · ')" aria-label="进入默认对话" :aria-description="[moduleDescription].filter(Boolean).join(' · ') || undefined" @click="emit('open-default')">
        <img class="brand-icon" :src="assistantIconUrl" alt="" />
        <div class="brand-copy">
          <strong>流萤<ModuleIndicator v-for="indicator in moduleIndicators.slice(0, 3)" :key="indicator.module_id" :indicator="indicator" /></strong>

          <span class="brand-caption">FIREFLY</span>
        </div>
      </button>
      <button v-if="!isMobile" class="button ghost icon sidebar-collapse-button" type="button" title="收起侧栏" aria-label="收起侧栏" :aria-expanded="!collapsed" @click="toggleCollapsed"><UiIcon name="sidebar" /></button>
    </div>
    <label v-if="isMobile" class="sidebar-search-field">
      <UiIcon name="search" />
      <input v-model="search" type="search" autocomplete="off" placeholder="搜索对话"
        aria-label="搜索对话" aria-controls="sidebar-conversation-list" />
    </label>
    <button v-else class="sidebar-search-button" type="button" aria-label="搜索对话"
      :title="`搜索对话 (${searchShortcutLabel})`" aria-haspopup="dialog" aria-keyshortcuts="Control+K Meta+K"
      :aria-expanded="searchOpen" aria-controls="conversation-search" @click="openSearch">
      <UiIcon name="search" /><span>搜索对话</span><kbd>{{ searchShortcutLabel }}</kbd>
    </button>
    <div class="panel-section workspace-section">
      <div v-if="error" class="panel-error" role="alert">{{ error }}</div>
      <div id="sidebar-conversation-list" ref="conversationList" class="conversation-list" :aria-busy="isLoading">
        <template v-for="group in conversationGroups" :key="group.id">
        <button
          v-if="group.id === 'archived'"
          class="conversation-show-more conversation-archive-toggle"
          type="button"
          :aria-expanded="showingArchived"
          aria-controls="sidebar-archived-conversations"
          :disabled="isLoading && !showingArchived"
          @click="emit('toggle-archived')"
        >
          <span>{{ showingArchived ? '收起归档对话' : '查看归档对话' }}</span>
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 6 3.5 3.5L11.5 6" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
        </button>
        <div v-show="group.id === 'active' || showingArchived" :id="`sidebar-${group.id}-conversations`"
          class="conversation-group" role="group" :aria-label="group.id === 'archived' ? '归档对话' : '当前对话'">
        <div v-if="isLoading && !group.conversations.length && (group.id === 'archived' || !showingArchived)" class="panel-loading" role="status">
          {{ group.id === 'archived' ? '加载归档对话…' : '加载中…' }}
        </div>
        <div v-else-if="!group.conversations.length && !error" class="panel-empty">
          {{ group.id === 'archived' ? (search.trim() ? '没有匹配的归档对话' : '暂无归档对话') : (search.trim() ? '没有匹配的对话' : '暂无对话') }}
        </div>
        <template v-for="(conversation, index) in group.conversations" :key="conversation.id">
        <div v-if="index === 0 && conversation.is_pinned && !conversation.is_archived"
          class="conversation-section-heading" role="heading" aria-level="2">
          <span class="conversation-section-label"><UiIcon name="pin" /><span>置顶</span></span>
          <span class="conversation-section-count" :aria-label="`${pinnedConversationCount} 条置顶对话`">{{ pinnedConversationCount }}</span>
        </div>
        <template v-if="!conversation.is_archived && !conversation.is_pinned && (index === 0 || group.conversations[index - 1].is_pinned)">
          <div v-if="index > 0" class="conversation-pin-divider" role="separator" aria-label="置顶对话与其他对话" />
          <div class="conversation-section-heading" :class="{ 'after-divider': index > 0 }" role="heading" aria-level="2">
            {{ search.trim() ? '搜索结果' : '最近对话' }}
          </div>
        </template>
        <div
          class="conversation-item"
          :class="{
            active: conversation.id === activeConversationId,
            pinned: conversation.is_pinned,
            running: conversation.status === 'running',
            unread: conversation.hasUnread && conversation.status === 'completed',
            confirming: deleteConfirmId === conversation.id,
            'menu-open': menuOpenId === conversation.id,
          }"
          role="button"
          tabindex="0"
          :title="!isMobile ? [formatConversationTitle(conversation), conversationAgentLabel(conversation), conversationDetail(conversation)].filter(Boolean).join(' · ') : undefined"
          @click="emit('select', conversation.id)"
          @keydown.enter.self.prevent="emit('select', conversation.id)"
          @keydown.space.self.prevent="emit('select', conversation.id)"
        >
          <span class="conversation-directory-prefix"
            :title="conversationCreationDirectoryPath(conversation)"
            :aria-label="`创建目录：${conversationCreationDirectoryPath(conversation)}`">
            <span class="conversation-directory-name">{{ conversationCreationDirectoryLabel(conversation) }}</span>
            <span v-if="!isMobile && conversation.id === activeConversationId" class="conversation-directory-agent">
              {{ conversationAgentLabel(conversation) }}
            </span>
          </span>
          <div class="conversation-info">
            <input
              v-if="editingConversationId === conversation.id"
              :ref="element => captureEditingTitleInput(element, conversation.id)"
              v-model="editingConversationTitle"
              class="conversation-title-input"
              type="text"
              maxlength="120"
              @click.stop
              @dblclick.stop
              @keydown.stop="handleTitleKeydown($event, conversation)"
              @blur="saveConversationTitle(conversation)"
            />
            <div
              v-else
              class="conversation-title"
              @dblclick.stop="startEditingConversation(conversation)"
            >
              <span class="conversation-title-text">{{ formatConversationTitle(conversation) }}</span>

              <span
                v-if="conversation.status === 'running'"
                class="conversation-running-spinner"
                title="正在生成"
                role="status"
                aria-label="正在生成"
              ></span>
              <ConversationUnreadBadge v-else-if="conversation.hasUnread && conversation.status === 'completed'" />
              <span v-if="!isMobile && conversation.hasDraft" class="draft-badge">草稿</span>
              <span v-if="!isMobile && conversation.workspace_mode === 'parallel'" class="draft-badge" :title="conversation.cwd">并行</span>
            </div>
          </div>
          <div class="conversation-actions" @keydown.stop>
            <button
              class="conversation-menu-trigger"
              type="button"
              title="更多操作"
              aria-label="更多操作"
              aria-haspopup="menu"
              :aria-expanded="menuOpenId === conversation.id"
              @click.stop="toggleMenu(conversation.id, $event)"
            >
              <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
                <circle cx="3" cy="8" r="1.3" fill="currentColor" />
                <circle cx="8" cy="8" r="1.3" fill="currentColor" />
                <circle cx="13" cy="8" r="1.3" fill="currentColor" />
              </svg>
            </button>
            <div
              v-if="menuOpenId === conversation.id"
              class="conversation-menu"
              :class="{ 'drop-up': menuDropUp }"
              role="menu"
              @keydown="handleMenuKeydown"
            >
              <button type="button" role="menuitem"
                :aria-busy="pinningConversationIds.includes(conversation.id)"
                :disabled="pinningConversationIds.includes(conversation.id)"
                @click.stop="dismissMenu(); emit('toggle-pin', conversation)"
              >{{ conversation.is_pinned ? '取消置顶' : '置顶对话' }}</button>
              <button
                type="button"
                role="menuitem"
                @mousedown.prevent
                @click.stop="onMenuRename(conversation)"
              >
                重命名
              </button>
              <button type="button" role="menuitem"
                :disabled="archivingConversationIds.includes(conversation.id) || (!conversation.is_archived && conversation.status === 'running')"
                @click.stop="dismissMenu(); emit('toggle-archive', conversation)">
                {{ conversation.is_archived ? '取消归档' : '归档' }}
              </button>
              <button
                type="button"
                role="menuitem"
                class="danger"
                aria-haspopup="true"
                :aria-expanded="deleteConfirmId === conversation.id"
                @click.stop="onMenuAction(conversation.id)"
              >
                删除
              </button>
            </div>
            <div
              v-if="deleteConfirmId === conversation.id"
              class="delete-bubble"
              :class="{ 'drop-up': menuDropUp }"
              role="dialog"
              aria-live="assertive"
              @keydown.esc.prevent="emit('dismiss-delete'); menuTrigger?.focus()"
            >
              <div class="delete-bubble-text">
                {{ conversation.workspace_mode === 'parallel' ? `确定永久删除这个对话吗？独立副本将保留在 ${conversation.cwd}。` : '确定永久删除这个对话吗？此操作不可撤销。' }}
              </div>
              <div class="delete-bubble-actions">
                <button class="button small ghost" type="button" @click.stop="emit('dismiss-delete')">取消</button>
                <button
                  class="button small danger"
                  type="button"
                  @click.stop="emit('delete', conversation.id)"
                >
                  永久删除
                </button>
              </div>
            </div>
          </div>
        </div>
        </template>
        </div>
        </template>
        <button
          v-if="showingArchived && hasMore && (!isLoading || conversationGroups[1].conversations.length)"
          class="conversation-show-more"
          type="button"
          :disabled="isLoading"
          @click="showMore"
        >
          <span>{{ isLoading ? '加载中…' : error ? '重试加载' : '查看更多对话' }}</span>
          <svg v-if="!isLoading" viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 6 3.5 3.5L11.5 6" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
        </button>
      </div>
    </div>
    <SidebarSettingsMenu class="sidebar-settings-footer" :active="!isMobile || open" :collapsed="collapsed"
      @logout="emit('logout')" />
  </aside>
</template>

<style scoped src="./conversation-sidebar.css"></style>
