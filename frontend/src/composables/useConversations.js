import { ref, computed, watch } from 'vue'
import { apiFetch } from '@/utils/http'

export function useConversations({ pendingConversation = null, isRenamingConversation = null,
  showingArchivedConversations = ref(false), expandArchivedOnMore = false } = {}) {
  const conversations = ref([])
  const activeConversationId = ref(null)
  const isLoadingConversations = ref(false)
  const conversationIds = ref([])
  const conversationNextCursor = ref(null)
  const workspaceSearch = ref('')
  const conversationQuery = computed(() => ({
    limit: 20, search: workspaceSearch.value, archived: expandArchivedOnMore ? false : showingArchivedConversations.value,
    ...(activeConversationId.value ? { selected_id: activeConversationId.value } : {}),
  }))
  let loadedArchived = false
  let loadedPages = 1
  let requestedPages = 1
  const archivedNextCursor = ref(null)
  const archivedLoaded = ref(false)
  let archivedPages = 0
  let requestedArchivedPages = 0
  const archivedConversationQuery = computed(() => ({ ...conversationQuery.value, archived: true }))
  const hasMoreConversations = computed(() => !!conversationNextCursor.value || (expandArchivedOnMore
    && (!showingArchivedConversations.value || !archivedLoaded.value || !!archivedNextCursor.value)))
  let conversationController = null
  let refreshingSnapshot = false
  let pendingSnapshot = null
  const conversationError = ref('')
  const deleteConfirmId = ref(null)
  const pinningConversationIds = ref([])
  const archivingConversationIds = ref([])
  let refreshPending = false
  let version = 0

  const activeConversation = computed(
    () => conversations.value.find((item) => item.id === activeConversationId.value) || pendingConversation?.value || null,
  )

  const formatConversationTitle = (conversation) => {
    if (!conversation) return '新的对话'
    if (typeof conversation.title === 'string' && conversation.title.trim()) return conversation.title.trim()
    return `对话 #${conversation.id}`
  }

  const formatConversationWorkspace = (conversation) => {
    if (!conversation || !['codex', 'claude'].includes(conversation.agent) || !conversation.cwd) return ''
    return `workspace/${conversation.cwd}`
  }

  const formatConversationMeta = (conversation) => {
    if (!conversation) return ''
    const agentNames = { firefly: 'Firefly', codex: 'Codex', claude: 'Claude' }
    const agent = agentNames[conversation.agent] || conversation.agent
    const workspace = formatConversationWorkspace(conversation)
    const cwd = workspace ? ` · ${workspace}` : ''
    const provider = String(conversation.codex_provider || '').trim()
    const model = String(conversation.codex_model || '').trim()
    const modelLabel = model && provider && model.startsWith(`${provider}/`) ? model.slice(provider.length + 1) : model
    const codex = conversation.agent === 'codex'
      ? ` · ${[provider || 'openai', modelLabel].filter(Boolean).join(' · ')}`
      : ''
    const claude = conversation.agent === 'claude' && conversation.claude_model ? ` · ${conversation.claude_model}` : ''
    return `${agent}${codex}${claude}${cwd}`
  }

  const patchConversation = (conversationId, patch = {}) => {
    if (!conversationId || !patch || typeof patch !== 'object') return false
    const index = conversations.value.findIndex((item) => item.id === conversationId)
    if (index === -1) return false
    const updated = { ...conversations.value[index], ...patch }
    conversations.value.splice(index, 1, updated)
    return true
  }

  const dismissDeleteConfirm = () => {
    deleteConfirmId.value = null
  }

  const toggleDeleteConfirm = (conversationId) => {
    if (!conversationId) return
    if (deleteConfirmId.value === conversationId) {
      dismissDeleteConfirm()
      return
    }
    deleteConfirmId.value = conversationId
  }

  const applyConversations = data => {
    if (!Array.isArray(data?.conversations)) return
    // A busy stream may push faster than later pages can load. Finish the
    // current refresh, then apply the latest snapshot instead of starving it.
    if (refreshingSnapshot && !isRenamingConversation?.value) {
      pendingSnapshot = { ...pendingSnapshot, firstPage: data }
      return
    }
    // A realtime snapshot covers the first page. Refresh the remaining loaded
    // pages before replacing the list, including after reconnects/deletions.
    return fetchConversations({ silent: true, firstPage: data })
  }

  const applyArchivedConversations = data => {
    if (!expandArchivedOnMore || !showingArchivedConversations.value || !Array.isArray(data?.conversations)) return
    if (refreshingSnapshot && !isRenamingConversation?.value) {
      pendingSnapshot = { ...pendingSnapshot, archivedFirstPage: data }
      return
    }
    return fetchConversations({ silent: true, archivedFirstPage: data })
  }

  const fetchConversations = async (options = {}) => {
    const { silent = false, more = false, archivesOnly = false, search = workspaceSearch.value,
      selectedId = activeConversationId.value, firstPage, archivedFirstPage } = options
    if (more) {
      if (isLoadingConversations.value || !hasMoreConversations.value) return false
      if (conversationController) {
        if (!refreshingSnapshot || isRenamingConversation?.value) return false
        // Extend a background refresh instead of dropping a user's load-more click.
        if (conversationNextCursor.value && !archivesOnly) requestedPages++
        if (expandArchivedOnMore) {
          showingArchivedConversations.value = true
          if (!archivedLoaded.value || archivedNextCursor.value) requestedArchivedPages++
        }
        isLoadingConversations.value = true
        conversationError.value = ''
        return true
      }
    }
    if (isRenamingConversation?.value) {
      version++
      refreshPending = true
      return false
    }
    const queryArchived = expandArchivedOnMore ? false : showingArchivedConversations.value
    if (search !== workspaceSearch.value || loadedArchived !== queryArchived) {
      loadedArchived = queryArchived
      workspaceSearch.value = search
      loadedPages = requestedPages = 1
      conversationIds.value = []
      conversationNextCursor.value = null
      archivedPages = requestedArchivedPages = 0
      archivedNextCursor.value = null
      archivedLoaded.value = false
    }
    if (expandArchivedOnMore && more) showingArchivedConversations.value = true
    conversationController?.abort()
    const controller = new AbortController()
    conversationController = controller
    const currentVersion = ++version
    const targetPages = more ? loadedPages + Number(!archivesOnly && !!conversationNextCursor.value) : requestedPages
    requestedPages = targetPages
    if (expandArchivedOnMore && showingArchivedConversations.value) {
      requestedArchivedPages = more
        ? archivedPages + Number(!archivedLoaded.value || !!archivedNextCursor.value)
        : Math.max(1, requestedArchivedPages)
    }
    refreshingSnapshot = !!(firstPage || archivedFirstPage) && (targetPages > 1 || requestedArchivedPages > 0)
    pendingSnapshot = null
    if (!silent) {
      isLoadingConversations.value = true
      conversationError.value = ''
    }
    try {
      const records = new Map(more
        ? conversations.value.filter(item => conversationIds.value.includes(item.id)).map(item => [item.id, item]) : [])
      let cursor = more ? conversationNextCursor.value : null
      let pageCount = more ? loadedPages : 0
      let data
      // When only archives have more pages, retain the loaded active partition.
      if (!more || (cursor && !archivesOnly)) do {
        if (!pageCount && firstPage) data = firstPage
        else {
          const query = new URLSearchParams({ limit: '20', search, archived: String(loadedArchived) })
          if (selectedId) query.set('selected_id', selectedId)
          if (cursor) for (const [key, value] of Object.entries(cursor)) query.set(key, value)
          const resp = await apiFetch(`/api/assistant/conversations?${query}`, {
            credentials: 'include', signal: controller.signal,
          })
          if (!resp.ok) throw new Error(await resp.text() || '加载对话失败')
          data = await resp.json()
        }
        if (currentVersion !== version) return false
        if (isRenamingConversation?.value) {
          refreshPending = true
          return false
        }
        for (const item of data.conversations || []) records.set(item.id, item)
        cursor = data.next_cursor || null
        pageCount++
      } while (cursor && pageCount < requestedPages)
      let archiveCursor = more ? archivedNextCursor.value : null
      let archivePageCount = more ? archivedPages : 0
      if (expandArchivedOnMore && showingArchivedConversations.value
        && (!more || !archivedLoaded.value || archiveCursor)) do {
        if (!archivePageCount && archivedFirstPage) data = archivedFirstPage
        else {
          const query = new URLSearchParams({ limit: '20', search, archived: 'true' })
          if (selectedId) query.set('selected_id', selectedId)
          if (archiveCursor) for (const [key, value] of Object.entries(archiveCursor)) query.set(key, value)
          const resp = await apiFetch(`/api/assistant/conversations?${query}`, {
            credentials: 'include', signal: controller.signal,
          })
          if (!resp.ok) throw new Error(await resp.text() || '加载已归档对话失败')
          data = await resp.json()
        }
        if (currentVersion !== version) return false
        if (isRenamingConversation?.value) {
          refreshPending = true
          return false
        }
        for (const item of data.conversations || []) records.set(item.id, item)
        archiveCursor = data.next_cursor || null
        archivePageCount++
      } while (archiveCursor && archivePageCount < requestedArchivedPages)
      conversationIds.value = [...records.keys()]
      // The selected record is separate from search results. A null selected
      // record is authoritative (deleted/expired), not a pagination miss.
      if (data?.selected_conversation) records.set(data.selected_conversation.id, data.selected_conversation)
      for (const item of conversations.value) {
        if (!records.has(item.id) && (item.id === activeConversationId.value && (item.message_count === 0 || item.id !== selectedId
            || !data || !('selected_conversation' in data)))) records.set(item.id, item)
      }
      conversations.value = [...records.values()]
      conversationNextCursor.value = cursor
      loadedPages = requestedPages = pageCount
      archivedPages = requestedArchivedPages = archivePageCount
      archivedNextCursor.value = archiveCursor
      archivedLoaded.value = archivePageCount > 0
      return true
    } catch (err) {
      if (currentVersion === version && (!silent || isLoadingConversations.value)) conversationError.value = err.message || '加载对话失败'
      return false
    } finally {
      if (conversationController === controller) {
        conversationController = null
        refreshingSnapshot = false
        requestedPages = loadedPages
        requestedArchivedPages = archivedPages
        isLoadingConversations.value = false
        const pending = pendingSnapshot
        pendingSnapshot = null
        if (pending) fetchConversations({ silent: true, ...pending })
      }
    }
  }

  if (isRenamingConversation) {
    watch(isRenamingConversation, (renaming, wasRenaming) => {
      if (renaming || !wasRenaming) return
      if (!refreshPending) return
      refreshPending = false
      fetchConversations({ silent: true })
    })
  }

  const createConversation = async (options = {}) => {
    const { title = '', agent = 'firefly', cwd = null, provider = null, model = null, reasoning_level = null, workspace_mode = 'shared' } = options || {}
    const payload = {}
    if (title && title.trim()) {
      payload.title = title.trim()
    }
    payload.agent = agent
    payload.workspace_mode = workspace_mode
    if (['codex', 'claude'].includes(agent) && cwd) payload.cwd = cwd
    if (agent === 'codex' && provider) payload.provider = provider
    if (['codex', 'claude'].includes(agent) && model) payload.model = model
    if (agent === 'codex' && reasoning_level) payload.reasoning_level = reasoning_level
    const resp = await apiFetch('/api/assistant/conversations', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify(payload),
    })
    if (!resp.ok) {
      const text = await resp.text()
      throw new Error(text || '创建工作失败')
    }
    return await resp.json()
  }

  const deleteConversation = async (conversationId) => {
    const resp = await apiFetch(`/api/assistant/conversations/${conversationId}`, {
      method: 'DELETE',
      credentials: 'include',
    })
    if (!resp.ok) {
      const text = await resp.text()
      let message = text
      try {
        message = JSON.parse(text)?.error || text
      } catch (_) {
        // Keep non-JSON error responses as-is.
      }
      throw new Error(message || '永久删除对话失败')
    }
  }

  const setConversationPinned = async (conversationId, isPinned) => {
    if (pinningConversationIds.value.includes(conversationId)) return false
    pinningConversationIds.value = [...pinningConversationIds.value, conversationId]
    try {
      const resp = await apiFetch(`/api/assistant/conversations/${conversationId}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'include',
        body: JSON.stringify({ is_pinned: isPinned }),
      })
      if (!resp.ok) throw new Error(await resp.text() || '保存置顶状态失败')
      patchConversation(conversationId, { is_pinned: isPinned })
      await fetchConversations({ silent: true })
      return true
    } finally {
      pinningConversationIds.value = pinningConversationIds.value.filter(id => id !== conversationId)
    }
  }

  const setConversationArchived = async (conversationId, archived) => {
    if (archivingConversationIds.value.includes(conversationId)) return false
    archivingConversationIds.value = [...archivingConversationIds.value, conversationId]
    try {
      const resp = await apiFetch(`/api/assistant/conversations/${conversationId}`, {
        method: 'PATCH', headers: { 'Content-Type': 'application/json' }, credentials: 'include',
        body: JSON.stringify({ is_archived: archived }),
      })
      if (!resp.ok) {
        const text = await resp.text()
        let message = text
        try { message = JSON.parse(text)?.error || text } catch (_) {}
        throw new Error(message || (archived ? '归档对话失败' : '取消归档失败'))
      }
      patchConversation(conversationId, { is_archived: archived })
      await fetchConversations({ silent: true })
      return true
    } finally {
      archivingConversationIds.value = archivingConversationIds.value.filter(id => id !== conversationId)
    }
  }

  const renameConversation = async (conversationId, title) => {
    const resp = await apiFetch(`/api/assistant/conversations/${conversationId}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ title }),
    })
    if (!resp.ok) {
      const text = await resp.text()
      throw new Error(text || '修改标题失败')
    }
    return await resp.json()
  }

  return {
    conversations,
    activeConversationId,
    isLoadingConversations,
    conversationIds,
    conversationNextCursor,
    conversationQuery,
    archivedConversationQuery,
    archivedLoaded,
    hasMoreConversations,
    conversationError,
    deleteConfirmId,
    pinningConversationIds,
    archivingConversationIds,
    setConversationArchived,
    activeConversation,
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
  }
}
