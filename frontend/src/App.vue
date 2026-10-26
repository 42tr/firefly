<script setup>
import { defineAsyncComponent, computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useAuth, loginReturnPath } from '@/composables/useAuth'
import LoginForm from './components/LoginForm.vue'
import { useConversations } from './composables/useConversations'
import { useMessages } from './composables/useMessages'
import { useAttachments } from './composables/useAttachments'
import { useConversationDrafts } from './composables/useConversationDrafts'
import { useConversationReadState } from './composables/useConversationReadState'
import { useWorkspace } from './composables/useWorkspace'
import { useMessageRendering } from './composables/useMessageRendering'
import { useMessageTimeline } from './composables/useMessageTimeline'
import { useChatStream } from './composables/useChatStream'
import { useBrowserNotifications } from './composables/useBrowserNotifications'
import { useMobileViewport } from './composables/useMobileViewport.js'
import { provideModules } from './composables/useModules'
import AmbientBackdrop from './components/AmbientBackdrop.vue'
import FireflyMoreMenu from './components/FireflyMoreMenu.vue'
import ProcessGroup from './components/ProcessGroup.vue'
import MarkdownMessage from './components/MarkdownMessage.vue'
import ToolOutput from './components/ToolOutput.vue'
import ConversationSidebar from './components/ConversationSidebar.vue'
import ConversationContext from './components/ConversationContext.vue'
import ScrollBottomButton from './components/ScrollBottomButton.vue'
import ChatInputBar from './components/ChatInputBar.vue'
// Dialogs are never part of first paint; splitting them out keeps the main
// chunk small and loads their chunks in parallel after boot.
const NewConversationDialog = defineAsyncComponent(() => import('./components/NewConversationDialog.vue'))
const ModelSettingsDialog = defineAsyncComponent(() => import('./components/ModelSettingsDialog.vue'))
const ScheduleDialog = defineAsyncComponent(() => import('./components/ScheduleDialog.vue'))
const MemoryDialog = defineAsyncComponent(() => import('./components/MemoryDialog.vue'))
const ImagePreview = defineAsyncComponent(() => import('./components/ImagePreview.vue'))
const WorkspaceFiles = defineAsyncComponent(() => import('./components/WorkspaceFiles.vue'))
import GoalCheckEvent from './components/GoalCheckEvent.vue'
import InputQueue from './components/InputQueue.vue'
import UiIcon from './components/UiIcon.vue'
import WorkflowTree from './components/WorkflowTree.vue'
import { provideAssistantRealtime, useRealtimeSubscription } from './composables/useAssistantRealtime.js'
import { apiFetch } from '@/utils/http'
import { conversationAgentLabel } from './utils/conversationLabels.js'
import { isVideoAttachment, isTextAttachment } from './composables/attachmentTypes.js'
import { useGoalMode } from './composables/useGoalMode.js'

// 仅在独立工作目录模式下渲染，按需加载，连带 composable 一起移出首屏主 chunk。
const ParallelWorkspaceStatus = defineAsyncComponent(() => import('./components/ParallelWorkspaceStatus.vue'))
const initialParams = new URL(window.location.href).searchParams
const modulePageOpen = ref(false)
const { viewportStyle } = useMobileViewport()
const imagePreview = ref(null)
const isStreaming = ref(false)
const sidebarOpen = ref(false)
const sidebarCollapsed = ref(false)
try { sidebarCollapsed.value = localStorage.getItem('firefly:sidebar-collapsed') === 'true' } catch { /* In-memory fallback. */ }
watch(sidebarCollapsed, async collapsed => {
  try { localStorage.setItem('firefly:sidebar-collapsed', String(collapsed)) } catch { /* In-memory fallback. */ }
  await nextTick()
  // 图表也需要响应侧栏导致的容器宽度变化。
  window.dispatchEvent(new Event('resize'))
})
const handleSidebarTransitionEnd = event => {
  // 宽度过渡结束后，图表需要按最终容器尺寸再调整一次。
  if (event.propertyName === 'grid-template-columns') window.dispatchEvent(new Event('resize'))
}
const pendingSettingsBusy = ref(false)
const newConversationKey = ref(0)
const lastUsedAgent = ref('firefly')
try {
  const saved = localStorage.getItem('firefly:last-agent')
  if (['firefly', 'codex', 'claude'].includes(saved)) lastUsedAgent.value = saved
} catch { /* In-memory preference when storage is unavailable. */ }
const isCreatingConversation = ref(false)
const newConversationPreset = ref(null)
const pendingConversation = ref(null)
const showScheduleDialog = ref(false)
const showMemoryDialog = ref(false)
const showModelSettings = ref(false)
const closeSidebar = () => { sidebarOpen.value = false }
const handleSidebarKeydown = (event) => {
  if (event.key !== 'Escape') return
  if (sidebarOpen.value) closeSidebar()
}
const { isReady, isChecking, isSubmitting, error: authError, check: checkLogin, login, logout } = useAuth()
const submitLogin = async password => {
  if (!await login(password)) return
  if (window.location.pathname === '/auth' || window.location.pathname === '/auth/') {
    window.location.replace(loginReturnPath(initialParams.get('url')))
    return
  }
  requestNotificationPermission()
  await ensureConversations()
}
provideAssistantRealtime(isReady)
const { invalidateIndicators } = provideModules(isReady)
const isRenamingConversation = ref(false)
const showingArchivedConversations = ref(false)
const {
  conversations,
  activeConversationId,
  isLoadingConversations,
  conversationIds,
  conversationQuery,
  archivedConversationQuery,
  archivedLoaded,
  hasMoreConversations,
  conversationError,
  activeConversation,
  deleteConfirmId,
  pinningConversationIds,
  archivingConversationIds,
  setConversationArchived,
  formatConversationTitle,
  formatConversationWorkspace,
  formatConversationMeta,
  patchConversation,
  dismissDeleteConfirm,
  toggleDeleteConfirm,
  fetchConversations,
  applyConversations,
  applyArchivedConversations,
  createConversation,
  deleteConversation,
  renameConversation,
  setConversationPinned,
} = useConversations({ pendingConversation, isRenamingConversation, showingArchivedConversations, expandArchivedOnMore: true })

watch(activeConversationId, () => { showModelSettings.value = false })
watch(pendingConversation, () => { showModelSettings.value = false })
const handleModelSettingsSaved = conversation => patchConversation(conversation.id, {
  codex_provider: conversation.codex_provider,
  codex_model: conversation.codex_model,
  codex_reasoning_level: conversation.codex_reasoning_level,
  claude_model: conversation.claude_model,
  claude_provider: conversation.claude_provider,
  claude_reasoning_level: conversation.claude_reasoning_level,
})

const { showWorkspaceFiles, readConversationIdFromUrl,
  activeWorkspaceLabel, activeWorkspaceDisplay } = useWorkspace({ activeConversationId, activeConversation })

const conversationSearch = ref('')
let workspaceInitialized = false
const filteredConversations = computed(() => {
  void draftRevision.value
  const loadedIds = new Set(conversationIds.value)
  return conversations.value
    .filter(item => !item.is_archived || showingArchivedConversations.value)
    .filter((item) => ((item.message_count ?? 0) > 0 || item.id === activeConversationId.value || inputDrafts.has(item.id)))
    .filter((item) => loadedIds.has(item.id) || ((item.id === activeConversationId.value || inputDrafts.has(item.id))
        && [formatConversationTitle(item), item.cwd || '', item.source_cwd || '', item.agent || ''].join(' ').toLocaleLowerCase().includes(conversationSearch.value.trim().toLocaleLowerCase())))
    .map((item) => ({
      ...item,
      hasDraft: !item.is_archived && inputDrafts.has(item.id),
      hasUnread: hasUnread(item),
      status: item.is_archived ? 'archived' : (item.status === 'running' || (isStreaming.value && item.id === activeConversationId.value) ? 'running' : 'completed'),
    }))
})
const visibleConversations = computed(() => [...filteredConversations.value]
  .sort((a, b) => Number(Boolean(a.is_archived)) - Number(Boolean(b.is_archived))
    || Number(Boolean(b.is_pinned)) - Number(Boolean(a.is_pinned))))
const refreshConversationSearch = () => fetchConversations({ search: conversationSearch.value.trim() })
const refreshRecentConversations = () => {
  showingArchivedConversations.value = false
  if (conversationSearch.value) conversationSearch.value = ''
  else fetchConversations({ search: '' })
}
const selectRecentConversation = id => {
  closeSidebar()
  selectConversation(id)
}
const openScheduledConversation = async id => {
  closeSidebar()
  const selectionToken = conversationSelectionToken
  if (!await fetchConversations({ silent: true, selectedId: id })) return
  if (selectionToken !== conversationSelectionToken) return
  await selectConversation(id, { force: true })
}
const openConversationSource = async () => {
  const id = conversationSource.value?.conversation_id
  if (!id) return
  const selectionToken = conversationSelectionToken
  if (!await fetchConversations({ silent: true, selectedId: id })) return
  if (selectionToken !== conversationSelectionToken) return
  const target = conversations.value.find(item => item.id === id)
  if (!target) {
    conversationError.value = '来源对话已删除'
    return
  }
  if (selectionToken !== conversationSelectionToken) return
  await selectConversation(id, { force: true })
}
watch(conversationSearch, () => {
  showingArchivedConversations.value = false
  fetchConversations({ search: conversationSearch.value.trim() })
})
const isArchivedConversation = computed(() => activeConversation.value?.is_archived === true)
// A deleted or expired conversation is absent from the authoritative snapshot.
const isMissingConversation = computed(() => !!activeConversationId.value && !activeConversation.value)
const activeAgentName = computed(() => {
  const agent = activeConversation.value?.agent
  return agent ? conversationAgentLabel(activeConversation.value) : 'Firefly'
})
const activeConversationTitle = computed(() => formatConversationTitle(activeConversation.value))
const {
  goal,
  status: goalStatus,
  error: goalError,
  enabledForNextMessage: goalEnabled,
  prepareSubmission: prepareGoalSubmission,
  setEnabled: setGoalEnabled,
  load: loadGoal,
} = useGoalMode({
  activeConversationId,
  onConversationPatched: next => {
    if (next?.status !== 'completed' || !next.last_check_result?.final_result || !activeConversationId.value) return
    if (messages.value.some(message => message.goal?.origin === 'goal_summary')) return
    void selectConversation(activeConversationId.value, { force: true })
  },
})
const goalCheckResult = computed(() => goal.value?.last_check_result || null)
const toggleGoalMode = async () => {
  if (goalEnabled.value) await setGoalEnabled(false)
  else goalEnabled.value = true
}
const prepareGoalMessage = async prompt => {
  const requested = goalEnabled.value
  const result = await prepareGoalSubmission(prompt)
  if (result === false && requested) {
    conversationError.value = goalError.value || '无法开启 Goal 模式，请重试'
    return false
  }
  return result ? { goalId: result } : null
}
const activeModelLabel = computed(() => {
  if (activeConversation.value?.agent === 'claude') {
    const model = String(activeConversation.value.claude_model || '').trim()
    const provider = String(activeConversation.value.claude_provider || '').trim()
    const level = String(activeConversation.value.claude_reasoning_level || '').trim()
    return ['Claude', provider, model, level].filter(Boolean).join(' · ')
  }
  if (activeConversation.value?.agent !== 'codex') return ''
  const provider = String(activeConversation.value.codex_provider || '').trim() || 'openai'
  const model = String(activeConversation.value.codex_model || '').trim()
  const modelLabel = model.startsWith(`${provider}/`) ? model.slice(provider.length + 1) : model
  const level = String(activeConversation.value.codex_reasoning_level || '').trim()
  return ['Codex', provider, modelLabel, level].filter(Boolean).join(' · ')
})
watch(sidebarOpen, (open) => {
  document.body.classList.toggle('sidebar-open', open)
})
const showMoreConversations = () => {
  if (isLoadingConversations.value || !hasMoreConversations.value) return
  return fetchConversations({ more: true })
}
const toggleArchivedConversations = () => {
  dismissDeleteConfirm()
  if (showingArchivedConversations.value) {
    showingArchivedConversations.value = false
    return
  }
  if (isLoadingConversations.value) return
  showingArchivedConversations.value = true
  if (!archivedLoaded.value) return fetchConversations({ more: true, archivesOnly: true })
}

const handleToggleConversationArchive = async (conversation) => {
  dismissDeleteConfirm()
  conversationError.value = ''
  try {
    if (!await setConversationArchived(conversation.id, !conversation.is_archived)) return
  } catch (err) {
    conversationError.value = err.message || '保存归档状态失败'
  }
}

const handleToggleConversationPin = async (conversation) => {
  conversationError.value = ''
  try {
    await setConversationPinned(conversation.id, !conversation.is_pinned)
  } catch (err) {
    conversationError.value = err.message || '保存置顶状态失败'
  }
}

const handleRenameConversation = async ({ conversation, title }) => {
  conversationError.value = ''
  try {
    const updated = await renameConversation(conversation.id, title)
    patchConversation(conversation.id, { title: updated.title ?? title })
  } catch (err) {
    conversationError.value = err.message || '修改标题失败'
  }
}

let conversationSelectionToken = 0
useRealtimeSubscription('conversations', conversationQuery, applyConversations, { enabled: isReady })
useRealtimeSubscription('conversations', archivedConversationQuery, applyArchivedConversations,
  { enabled: computed(() => isReady.value && showingArchivedConversations.value) })

const {
  messages,
  isLoadingMessages,
  chatPanel,
  isPinnedToBottom,
  welcomeMessage,
  scrollToBottom,
  vStableReply,
  normalizeAttachments,
  transformServerMessages,
  loadConversationMessages,
  renderCharts,
  disposeAllCharts,
} = useMessages()

// 每页按「渲染气泡数」计（后端分页已把工具调用展开计入），而不是数据库记录数
const messagePageSize = 20
const hasMoreMessages = ref(false)
const nextBeforeMessageId = ref(null)
const isLoadingMoreMessages = ref(false)
const isSwitchingConversation = ref(false)
const messageLoadError = ref('')
const conversationSource = ref(null)
const { hasUnread, forget: forgetConversationReadState } = useConversationReadState({
  conversations, activeConversationId,
  readMessageId: computed(() => messages.value.reduce((latest, message) => Math.max(latest,
    message.readMessageId || 0, message.messageId || 0, message.processRange?.end_id || 0), 0)),
  saveRead: async (id, messageId) => {
    const response = await apiFetch(`/api/assistant/conversations/${id}/read`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ message_id: messageId }),
    })
    if (!response.ok) throw new Error('同步已读状态失败')
    return (await response.json()).read_message_id
  },
  isReading: computed(() => !modulePageOpen.value && !isSwitchingConversation.value
    && !isLoadingMessages.value && !messageLoadError.value && messages.value.length > 0
    && isPinnedToBottom.value),
})
const inputDisabled = computed(() => isCreatingConversation.value || isArchivedConversation.value || isMissingConversation.value || isSwitchingConversation.value || !!messageLoadError.value || (!activeConversationId.value && !pendingConversation.value)
  || (activeConversation.value?.workspace_mode === 'parallel' && ['failed', 'cancelled', 'missing'].includes(activeConversation.value.workspace_state)))

const { requestPermission: requestNotificationPermission, notifyConversationCompleted } = useBrowserNotifications({
  onOpenConversation: (conversationId) => selectConversation(conversationId),
})
let knownConversationStatuses = new Map()
watch(conversations, (items) => {
  for (const conversation of items) {
    if (knownConversationStatuses.get(conversation.id) === 'running' && conversation.status === 'completed') {
      notifyConversationCompleted(conversation)
    }
  }
  knownConversationStatuses = new Map(items.map((conversation) => [conversation.id, conversation.status]))
}, { deep: true })

const {
  chatAttachments,
  attachmentError,
  messageAttachmentUrls,
  releaseMessageAttachmentUrls,
  clearAttachments,
  removeAttachment,
  handleAttachmentChange,
  handlePaste,
} = useAttachments()


let pendingCreationPromise = null
const ensurePendingConversation = async () => {
  if (pendingCreationPromise) return pendingCreationPromise
  const pending = pendingConversation.value
  if (!pending) return null
  const selectionToken = conversationSelectionToken
  isCreatingConversation.value = true
  pendingCreationPromise = (async () => {
    const created = await createConversation(pending)
    if (!created?.id) throw new Error('创建对话失败，请重试')
    conversations.value = [created, ...conversations.value]
    if (selectionToken !== conversationSelectionToken || pendingConversation.value !== pending) return created
    pendingConversation.value = null
    activeConversationId.value = created.id
    return created
  })()
  try {
    return await pendingCreationPromise
  } finally {
    pendingCreationPromise = null
    isCreatingConversation.value = false
  }
}

const {
  input,
  sendMessage,
  retryMessage,
  getLocalSubmissions,
  disposeLocalSubmissions,
  abortStream,
  stopGeneration,
  resumeJob,
  queuedMessages,
  removeQueuedMessage,
} = useChatStream({
  messages,
  activeConversationId,
  isStreaming,
  conversationError,
  scrollToBottom,
  normalizeAttachments,
  chatAttachments,
  messageAttachmentUrls,
  clearAttachments,
  fetchConversations,
  patchConversation,
  ensureConversation: ensurePendingConversation,
  canSend: () => !inputDisabled.value && !pendingSettingsBusy.value,
  onModuleToolChanged: invalidateIndicators,
  transformServerMessages,
  onMessagesRestored: renderCharts,
  beforeSend: prepareGoalMessage,
})

const { inputDrafts, draftRevision, restoreDraft } = useConversationDrafts({
  activeConversationId, input, chatAttachments,
})

const { renderMessageHtml, handleChatPanelClick, resolveImageUrl } = useMessageRendering({
  activeConversation, isStreaming, imagePreview,
})

const { groupedMessages, loadingProcessJobs, processDetailErrors, loadProcessDetail } = useMessageTimeline({
  messages, activeConversationId, transformServerMessages,
})

const statusText = computed(() => {
  if (isCreatingConversation.value) return '正在创建对话…'
  if (isLoadingMessages.value) return '历史同步中…'
  if (isStreaming.value) {
    // Scan from the end without copying the array twice per delta.
    let runningStep = null
    let lastMessage = null
    for (let i = messages.value.length - 1; i >= 0; i -= 1) {
      const message = messages.value[i]
      if (!runningStep && message.status === 'running' && ['tool-run', 'handoff'].includes(message.kind)) runningStep = message
      if (!lastMessage && !['submitting', 'queued'].includes(message.processingStatus)) lastMessage = message
      if (runningStep && lastMessage) break
    }
    const phase = runningStep
      ? `正在执行：${runningStep.name || runningStep.agent || '工具'}`
      : lastMessage?.role === 'assistant' && lastMessage.content ? '正在回复…' : '正在思考中…'
    return phase
  }
  if (queuedMessages.value.length) return '等待处理…'
  if (isArchivedConversation.value) return '已归档 · 仅供查看'
  return '就绪'
})

const isPreviewableAttachment = (attachment) =>
  Boolean(attachment?.previewUrl || (attachment?.url && (attachment?.type?.startsWith('image/') || isVideoAttachment(attachment) || isTextAttachment(attachment))))

const getAttachmentPreviewUrl = (attachment) => attachment.previewUrl || attachment.url || ''

const previewAttachment = (attachment) => {
  if (!isPreviewableAttachment(attachment)) return
  const url = getAttachmentPreviewUrl(attachment)
  if (!url) return
  if (isVideoAttachment(attachment)) {
    imagePreview.value?.openVideo(url, attachment.name)
  } else if (attachment.type?.startsWith('image/') || attachment.previewUrl) {
    imagePreview.value?.open(url, attachment.name)
  } else {
    imagePreview.value?.openText(url, attachment.name)
  }
}

const formatToolValue = (value) => {
  if (value === null || value === undefined) return ''
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value, null, 2)
  } catch (err) {
    return String(value)
  }
}

const toolStatusText = (status) => {
  if (status === 'completed') return '已完成'
  if (status === 'error') return '失败'
  if (status === 'cancelled') return '已停止'
  if (status === 'interrupted') return '结果未知'
  return '执行中'
}

const formatFileSize = (bytes = 0) => {
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(2)} MB`
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${bytes} B`
}

const selectConversation = async (conversationId, options = {}) => {
  if (!conversationId) return
  pendingConversation.value = null
  pendingSettingsBusy.value = false
  closeSidebar()
  dismissDeleteConfirm()
  if (!options.force && activeConversationId.value === conversationId && messages.value.length) return

  // Disconnect the visible stream. The server-side generation remains active
  // and will be resumed when this conversation is selected again.
  if (isStreaming.value) abortStream()
  disposeAllCharts()

  const selectionToken = ++conversationSelectionToken
  isSwitchingConversation.value = true
  messageLoadError.value = ''
  conversationSource.value = null
  restoreDraft(conversationId)
  attachmentError.value = ''
  releaseMessageAttachmentUrls()
  messages.value = []
  conversationError.value = ''
  hasMoreMessages.value = false
  nextBeforeMessageId.value = null
  try {
    const page = await loadConversationMessages(conversationId, { limit: messagePageSize })
    if (selectionToken !== conversationSelectionToken || activeConversationId.value !== conversationId) return
    const records = page?.messages || []
    conversationSource.value = page?.sourceReply || null
    hasMoreMessages.value = page?.hasMore === true
    nextBeforeMessageId.value = page?.nextBefore ?? null
    const history = transformServerMessages(records || [])
    // 旧会话消息里的本地预览 blob URL 随消息替换不再使用，统一释放
    releaseMessageAttachmentUrls()
    const localMessages = getLocalSubmissions(conversationId)
    messages.value = [...history, ...localMessages]
    if (!messages.value.length) messages.value = welcomeMessage()
    void loadGoal(conversationId)
    await scrollToBottom({ force: true })
    await renderCharts()

    if (activeConversation.value?.is_archived) return

    // 检查是否有进行中的生成任务，有则自动续连
    try {
      const activeResp = await apiFetch(`/api/assistant/conversations/${conversationId}/active-generation`, {
        credentials: 'include',
      })
      if (activeResp.ok) {
        const activeData = await activeResp.json()
        if (selectionToken !== conversationSelectionToken || activeConversationId.value !== conversationId) return
        if (activeData.active && activeData.job_id) {
          patchConversation(conversationId, { status: 'running' })
          isStreaming.value = true
          resumeJob(activeData.job_id, conversationId)
        } else {
          patchConversation(conversationId, { status: 'completed' })
        }
      }
    } catch (err) {
      console.error('Failed to check active generation', err)
    }
  } catch (err) {
    if (selectionToken !== conversationSelectionToken || activeConversationId.value !== conversationId) return
    messageLoadError.value = err.message || '加载历史失败'
    releaseMessageAttachmentUrls()
    messages.value = []
  } finally {
    if (selectionToken === conversationSelectionToken) isSwitchingConversation.value = false
  }
}

const showMoreMessages = async () => {
  const conversationId = activeConversationId.value
  const before = nextBeforeMessageId.value
  if (!conversationId || !hasMoreMessages.value || !before || isLoadingMoreMessages.value) return

  const selectionToken = conversationSelectionToken
  const panel = chatPanel.value
  const previousScrollTop = panel?.scrollTop || 0
  const previousScrollHeight = panel?.scrollHeight || 0
  isLoadingMoreMessages.value = true
  try {
    const page = await loadConversationMessages(conversationId, { limit: messagePageSize, before })
    if (selectionToken !== conversationSelectionToken || activeConversationId.value !== conversationId) return
    const olderHistory = transformServerMessages(page?.messages || [])
    if (olderHistory.length) {
      messages.value = [...olderHistory, ...messages.value]
      await nextTick()
      if (panel) panel.scrollTop = previousScrollTop + panel.scrollHeight - previousScrollHeight
    }
    hasMoreMessages.value = page?.hasMore === true
    nextBeforeMessageId.value = page?.nextBefore ?? null
    await renderCharts()
  } catch (err) {
    if (selectionToken === conversationSelectionToken && activeConversationId.value === conversationId) {
      conversationError.value = err.message || '加载更多记录失败'
    }
  } finally {
    isLoadingMoreMessages.value = false
  }
}

const ensureConversations = async () => {
  const id = readConversationIdFromUrl()
  // Include an old linked workspace in the initial realtime subscription too.
  if (id) activeConversationId.value = id
  await fetchConversations({ selectedId: id })
  workspaceInitialized = true
  if (new URL(window.location.href).searchParams.get('panel') !== 'publications' && id && conversations.value.some(item => item.id === id)) {
    await selectConversation(id, { force: true })
  } else await openNewConversationDialog()
}

const openDefaultView = async () => {
  closeSidebar()
  await openDefaultConversation()
  if (!workspaceInitialized) {
    await fetchConversations()
    workspaceInitialized = true
  }
}

const openNewConversationDialog = async () => {
  if (isCreatingConversation.value) return
  const agent = activeConversation.value?.agent || lastUsedAgent.value
  newConversationPreset.value = null
  closeSidebar()
  await handleCreateConversation({ agent, workspace_mode: 'shared' })
}
const openWorkspaceConversationDialog = async conversation => {
  if (isCreatingConversation.value) return
  newConversationPreset.value = { ...conversation }
  await handleCreateConversation({
    agent: conversation.agent, cwd: conversation.cwd,
    workspace_mode: conversation.workspace_mode,
    provider: conversation.agent === 'claude' ? conversation.claude_provider : conversation.codex_provider,
    model: conversation.agent === 'claude' ? conversation.claude_model : conversation.codex_model,
    reasoning_level: conversation.agent === 'claude' ? conversation.claude_reasoning_level : conversation.codex_reasoning_level,
  })
}
const updatePendingConversation = options => {
  if (!pendingConversation.value || isCreatingConversation.value) return
  pendingConversation.value = { ...pendingConversation.value, ...options }
  lastUsedAgent.value = options.agent
  try { localStorage.setItem('firefly:last-agent', options.agent) } catch { /* In-memory fallback. */ }
}
const handleWorkspaceRetried = async id => {
  await fetchConversations({ silent: true })
  if (activeConversationId.value === id) await selectConversation(id, { force: true })
}
const handleCreateConversation = async (options = {}) => {
  if (isCreatingConversation.value) return
  isCreatingConversation.value = true
  conversationError.value = ''
  try {
    newConversationKey.value += 1
    pendingConversation.value = {
      ...options,
      title: options.title || '新的对话',
      message_count: 0,
      status: 'completed',
      is_archived: false,
    }
    pendingSettingsBusy.value = false
    abortStream()
    ++conversationSelectionToken
    isSwitchingConversation.value = false
    messageLoadError.value = ''
    conversationSource.value = null
    conversationError.value = ''
    hasMoreMessages.value = false
    nextBeforeMessageId.value = null
    disposeAllCharts()
    releaseMessageAttachmentUrls()
    restoreDraft(null)
    messages.value = []
    input.value = ''
    clearAttachments()
    closeSidebar()
    dismissDeleteConfirm()
    activeConversationId.value = null
    if (showingArchivedConversations.value) {
      showingArchivedConversations.value = false
      fetchConversations({ silent: true })
    }
    const url = new URL(window.location.href)
    url.searchParams.delete('conversation')
    window.history.replaceState(null, '', url)
    await scrollToBottom({ force: true })
  } catch (err) {
    conversationError.value = err.message || '创建对话失败'
  } finally {
    isCreatingConversation.value = false
  }
}

const openDefaultConversation = () => {
  newConversationPreset.value = null
  return handleCreateConversation({ agent: 'firefly', workspace_mode: 'shared' })
}

const handleDeleteConversation = async (conversationId) => {
  if (!conversationId) return
  dismissDeleteConfirm()
  conversationError.value = ''
  try {
    await deleteConversation(conversationId)
    forgetConversationReadState(conversationId)
    inputDrafts.delete(conversationId)
    conversations.value = conversations.value.filter((item) => item.id !== conversationId)
    if (activeConversationId.value === conversationId) {
      activeConversationId.value = null
      const nextConversation = conversations.value[0]
      if (nextConversation) {
        await selectConversation(nextConversation.id, { force: true })
      } else {
        // 没有其它对话时进入新的流萤对话
        await openDefaultConversation()
        if (!activeConversationId.value) {
          disposeAllCharts()
          messages.value = []
        }
      }
    }
  } catch (err) {
    conversationError.value = err.message || '永久删除对话失败'
  }
}

const handleDeleteBubbleOutside = (event) => {
  const target = event.target
  if (!target || typeof target.closest !== 'function') return
  if (target.closest('.conversation-actions')) return
  dismissDeleteConfirm()
}

const doCheckLogin = async () => {
  await checkLogin()
  if (isReady.value) {
    if (window.location.pathname === '/auth' || window.location.pathname === '/auth/') {
      window.location.replace(loginReturnPath(initialParams.get('url')))
      return
    }
    requestNotificationPermission()
    await ensureConversations()
  }
}

onBeforeUnmount(() => {
  disposeLocalSubmissions()
  abortStream()
  disposeAllCharts()
  releaseMessageAttachmentUrls()
  clearAttachments()
  document.removeEventListener('pointerdown', handleDeleteBubbleOutside)
  document.removeEventListener('keydown', handleSidebarKeydown)
  document.body.classList.remove('sidebar-open')
})

onMounted(() => {
  document.addEventListener('pointerdown', handleDeleteBubbleOutside)
  document.addEventListener('keydown', handleSidebarKeydown)
  doCheckLogin()
})
</script>

<template>
  <div class="app-shell" :style="viewportStyle">
    <AmbientBackdrop />
    <div v-if="!isReady" class="view">
      <LoginForm :checking="isChecking" :submitting="isSubmitting" :error="authError" @login="submitLogin" @retry="doCheckLogin" />
    </div>

    <div v-else class="view">
      <p v-if="authError" class="auth-error" role="alert">{{ authError }}</p>
      <div class="panel-grid" :class="{ 'sidebar-collapsed': sidebarCollapsed }" @transitionend.self="handleSidebarTransitionEnd">
        <ConversationSidebar
          @logout="logout"
          :open="sidebarOpen"
          :collapsed="sidebarCollapsed"
          :conversations="visibleConversations"
          :recent-conversations="conversations"
          v-model:search="conversationSearch"
          :active-conversation-id="activeConversationId"
          :is-loading="isLoadingConversations"
          :error="conversationError"
          :delete-confirm-id="deleteConfirmId"
          :pinning-conversation-ids="pinningConversationIds"
          :archiving-conversation-ids="archivingConversationIds"
          :has-more="hasMoreConversations"
          :showing-archived="showingArchivedConversations"
          :format-conversation-title="formatConversationTitle"
          :format-conversation-workspace="formatConversationWorkspace"
          :format-conversation-meta="formatConversationMeta"
          @select="selectConversation"
          @delete="handleDeleteConversation"
          @toggle-delete="toggleDeleteConfirm"
          @dismiss-delete="dismissDeleteConfirm"
          @show-more="showMoreConversations"
          @toggle-archived="toggleArchivedConversations"
          @rename="handleRenameConversation"
          @toggle-pin="handleToggleConversationPin"
          @toggle-archive="handleToggleConversationArchive"
          @rename-start="isRenamingConversation = true"
          @rename-end="isRenamingConversation = false"
          @open-default="openDefaultView"
          @toggle-collapse="sidebarCollapsed = !sidebarCollapsed"
          @close="closeSidebar"
          @refresh-recent="refreshRecentConversations"
          @refresh-search="refreshConversationSearch"
          @select-recent="selectRecentConversation"
        />

        <Transition name="sidebar-backdrop">
          <div v-if="sidebarOpen" class="sidebar-backdrop" @click="closeSidebar"></div>
        </Transition>

        <div class="conversation-column" :class="{ 'coding-workspace': !pendingConversation && ['codex', 'claude'].includes(activeConversation?.agent), 'new-conversation-page': !!pendingConversation }">
          <div class="conversation-toolbar">
            <div class="toolbar-left">
              <button
                class="button ghost icon mobile-sidebar-toggle"
                data-icon-motion="sidebar"
                type="button"
                @click="sidebarOpen = !sidebarOpen"
                aria-label="展开侧边栏"
                :aria-expanded="sidebarOpen"
              >
                <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 4h11M2.5 8h11M2.5 12h11" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" /></svg>
              </button>
              <button
                class="button ghost icon toolbar-create-button"
                data-icon-motion="compose"
                type="button"
                title="新对话"
                aria-label="新对话"
                :disabled="isCreatingConversation"
                @click="openNewConversationDialog"
              ><UiIcon name="compose" /></button>
            </div>
            <div v-if="pendingConversation" class="toolbar-context new-conversation-title">新对话</div>
            <ConversationContext v-else :title="activeConversationTitle" :agent="activeConversation?.agent" :agent-label="activeModelLabel || activeAgentName" :workspace="activeWorkspaceDisplay" :workspace-label="activeWorkspaceLabel" />
            <div class="toolbar-actions">
              <button v-if="!pendingConversation && ['codex', 'claude'].includes(activeConversation?.agent)" class="button ghost icon" data-icon-motion="sliders" type="button" title="模型设置" aria-label="模型设置" :disabled="isArchivedConversation || isSwitchingConversation" @click="showModelSettings = true"><UiIcon name="sliders" /></button>
              <FireflyMoreMenu @module-open="modulePageOpen = $event" :memory-open="showMemoryDialog" :schedules-open="showScheduleDialog" :files-open="showWorkspaceFiles" :show-files="!pendingConversation && ['codex', 'claude'].includes(activeConversation?.agent)" :files-disabled="activeConversation?.workspace_mode === 'parallel' && activeConversation?.workspace_state !== 'ready'" @open-memory="showMemoryDialog = true" @open-schedules="showScheduleDialog = true" @open-files="showWorkspaceFiles = true">
              </FireflyMoreMenu>
            </div>
          </div>
          <ParallelWorkspaceStatus v-if="!pendingConversation && activeConversation?.workspace_mode === 'parallel'" :key="activeConversationId || 'draft'" :conversation="activeConversation" @retried="handleWorkspaceRetried" @create="openWorkspaceConversationDialog" />
          <div v-if="isSwitchingConversation" class="history-loading" role="status">正在加载对话…</div>
          <div v-if="conversationSource" class="conversation-source">
            <span>承接历史回复：{{ conversationSource.preview }}</span>
            <button v-if="conversationSource.conversation_id" type="button" class="button ghost small" @click="openConversationSource">查看来源对话</button>
          </div>
          <div class="chat-panel-wrap">
          <div ref="chatPanel" class="chat-panel" :class="{ 'new-chat-panel': pendingConversation && !messages.length }" @click="handleChatPanelClick">
            <div class="chat-inner">
              <div v-if="pendingConversation && !messages.length" class="new-conversation-empty">
                <h1>想聊点什么？</h1>
                <p>选择助手，开始新的对话</p>
              </div>
              <div v-if="isSwitchingConversation" class="chat-state" role="status">正在加载对话…</div>
              <div v-else-if="messageLoadError" class="chat-state" role="alert">
                <p>加载对话失败：{{ messageLoadError }}</p>
                <button class="button ghost small" type="button" @click="selectConversation(activeConversationId, { force: true })">重试</button>
              </div>
              <button
                v-if="hasMoreMessages"
                class="load-more-messages"
                type="button"
                :disabled="isLoadingMoreMessages"
                @click="showMoreMessages"
              >
                {{ isLoadingMoreMessages ? '正在加载…' : '查看更多记录' }}
              </button>
              <template v-for="item in groupedMessages" :key="item.key">
              <time v-if="item.timeLabel" class="message-time-divider" :datetime="item.timeValue" :title="item.timeTitle">{{ item.timeLabel }}</time>
              <ProcessGroup
                v-if="item.type === 'process'"
                :conversation-id="activeConversationId"
                :job-id="item.jobId"
                :steps="item.steps"
                :timing-steps="item.timingSteps"
                :show-duration="item.showDuration"
                :running="item.running"
                :loading="loadingProcessJobs.has(item.detailKey)"
                :error="processDetailErrors.get(item.detailKey) || ''"
                :tool-status-text="toolStatusText"
                :format-tool-value="formatToolValue"
                :render-message-html="renderMessageHtml"
                :resolve-image-url="resolveImageUrl"
                @expand="loadProcessDetail(item)"
              />
              <div
                v-else
                v-memo="[item.key, item.messageKey, item.latestReply, item.msg, item.msg.content, item.msg.status,
                  item.msg.processingStatus, item.msg.processingError, item.msg.attachments, item.msg.detailOmitted,
                  item.msg.chartOption, item.msg.imageOutput, item.msg.goal]"
                v-stable-reply="item.latestReply ? item.msg : null"
                class="message"
                :class="[item.msg.role, item.msg.chartOption ? 'chart' : '', item.msg.kind || '', { 'latest-reply': item.latestReply, 'goal-message': Boolean(item.msg.goal), 'goal-reply': item.msg.goal?.origin === 'goal_reply', 'goal-continuation': item.msg.goal?.origin === 'goal_continue', 'goal-final-summary': item.msg.goal?.origin === 'goal_summary' }]"
              >
              <div class="message-content">
                <div v-if="item.msg.goal" class="goal-message-label"><UiIcon name="target" />{{ item.msg.goal.origin === 'goal_summary' ? 'Goal 最终结果' : item.msg.goal.origin === 'goal_continue' ? 'Goal 自动续接' : item.msg.goal.origin === 'goal_reply' ? 'Goal 执行回复' : 'Goal 目标消息' }}</div>
                <div v-if="item.msg.title" class="message-title">{{ item.msg.title }}</div>
                <div v-if="item.msg.chartOption" :id="item.msg.chartId" class="chart-card"></div>
                <ToolOutput v-else-if="item.msg.imageOutput" :value="item.msg.imageOutput" :resolve-image-url="resolveImageUrl" />
                <MarkdownMessage
                  v-else-if="item.msg.role === 'assistant'"
                  :key="item.messageKey"
                  :html="renderMessageHtml(item.msg)"
                />
                <div v-else class="plain-text">{{ item.msg.content }}</div>
                <div v-if="item.msg.processingStatus === 'failed'" class="message-send-error" role="alert">
                  <span>{{ item.msg.processingError === '用户已停止生成' ? '已停止当前回复' : `${item.msg.retryAttachments ? '发送失败' : '执行失败'}：${item.msg.processingError || '请稍后重试'}` }}</span>
                  <button v-if="item.msg.retryAttachments" type="button" class="button ghost small" :disabled="inputDisabled" @click="retryMessage(item.msg)">重试</button>
                </div>
                <div v-if="item.msg.attachments?.length" class="message-attachments">
                  <component
                    :is="isPreviewableAttachment(attachment) ? 'button' : 'div'"
                    v-for="attachment in item.msg.attachments"
                    :key="attachment.id || attachment.name"
                    class="attachment-item"
                    :type="isPreviewableAttachment(attachment) ? 'button' : undefined"
                    :aria-label="isPreviewableAttachment(attachment) ? `预览文件：${attachment.name}` : undefined"
                    @click="previewAttachment(attachment)"
                  >
                    <span
                      v-if="attachment.type?.startsWith('image/')"
                      class="attachment-thumb"
                    >
                      <img
                        :src="getAttachmentPreviewUrl(attachment)"
                        :alt="attachment.name"
                      />
                    </span>
                    <span v-else class="attachment-file-icon"><UiIcon :name="isVideoAttachment(attachment) ? 'video' : 'attachment'" /></span>
                    <div class="attachment-info">
                      <div class="attachment-name" :title="attachment.name">{{ attachment.name }}</div>
                      <div class="attachment-size">{{ formatFileSize(attachment.size) }}</div>
                    </div>
                  </component>
                </div>
              </div>
              </div>
              <WorkflowTree
                v-if="activeConversation?.agent === 'firefly' && item.type === 'msg' && item.msg.role === 'user' && item.msg.generationJobId"
                v-memo="[item.msg.generationJobId, item.msg.processingStatus]"
                :conversation-id="activeConversationId"
                :job-id="item.msg.generationJobId"
                :reply-status="item.msg.processingStatus"
              />
              </template>
              <GoalCheckEvent :result="goalCheckResult" />
            </div>
          </div>
          <ScrollBottomButton
            v-if="!isPinnedToBottom"
            @click="scrollToBottom({ force: true })"
          />
          </div>

          <div v-if="!isArchivedConversation" class="composer-utilities">
            <div class="composer-utilities-inner">
              <InputQueue
                v-if="queuedMessages.length"
                :messages="queuedMessages"
                @remove="removeQueuedMessage"
              />
              <span class="input-status" :class="{ 'input-status-idle': statusText === '就绪' }" role="status">
                <span v-if="isStreaming" class="input-status-dot"></span>
                <span class="input-status-text" :title="statusText">{{ statusText }}</span>
              </span>
            </div>
          </div>

          <div v-if="isArchivedConversation" class="archived-conversation-notice">
            <span>此对话已归档</span>
            <button class="button ghost" type="button" :disabled="archivingConversationIds.includes(activeConversationId)"
              @click="handleToggleConversationArchive(activeConversation)">取消归档并继续</button>
          </div>
          <ChatInputBar v-else
            v-model="input"
            :attachments="chatAttachments"
            :attachment-error="attachmentError"
            :is-streaming="isStreaming"
            :has-queued-messages="queuedMessages.length > 0"
            :disabled="inputDisabled"
            :send-disabled="pendingSettingsBusy"
            :disabled-message="pendingSettingsBusy ? '正在准备工作目录或模型…' : isMissingConversation ? '该对话已删除' : isSwitchingConversation ? '正在加载对话…' : messageLoadError ? '请先重试加载对话' : '请先选择一个对话'"
            :goal-status="goalStatus"
            :goal-enabled="goalEnabled"
            @send="sendMessage"
            @stop="stopGeneration"
            @toggle-goal="toggleGoalMode"
            @remove-attachment="removeAttachment"
            @attachment-change="handleAttachmentChange"
            @attachment-paste="handlePaste"
          >
            <template v-if="pendingConversation" #controls>
              <p v-if="conversationError" class="creation-send-error" role="alert">{{ conversationError }}</p>
              <NewConversationDialog :key="newConversationKey" :open="true" :default-agent="pendingConversation.agent" :preset="newConversationPreset" :creating="isCreatingConversation" @change="updatePendingConversation" @busy="pendingSettingsBusy = $event" />
            </template>
          </ChatInputBar>
        </div>
      </div>
      <ModelSettingsDialog :open="showModelSettings" :conversation="activeConversation" @close="showModelSettings = false" @saved="handleModelSettingsSaved" />
      <MemoryDialog :open="showMemoryDialog" @close="showMemoryDialog = false" />
      <ScheduleDialog :open="showScheduleDialog" @close="showScheduleDialog = false" @open-conversation="openScheduledConversation" />
      <ImagePreview ref="imagePreview" />
      <WorkspaceFiles :open="showWorkspaceFiles" :conversation="activeConversation" @close="showWorkspaceFiles = false" />
    </div>
  </div>
</template>

<style scoped src="./app.css"></style>
