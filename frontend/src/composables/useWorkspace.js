import { computed, ref, watch } from 'vue'

export function useWorkspace({ activeConversationId, activeConversation }) {
  const showWorkspaceFiles = ref(false)
  // 地址栏同步当前对话 id：点击对话后更新 URL，刷新页面时恢复到该对话
  const CONVERSATION_URL_PARAM = 'conversation'
  const readConversationIdFromUrl = () => {
    try {
      const id = Number(new URL(window.location.href).searchParams.get(CONVERSATION_URL_PARAM))
      return Number.isInteger(id) && id > 0 ? id : null
    } catch {
      return null
    }
  }
  const syncConversationUrl = (conversationId) => {
    try {
      const url = new URL(window.location.href)
      if (conversationId == null) url.searchParams.delete(CONVERSATION_URL_PARAM)
      else url.searchParams.set(CONVERSATION_URL_PARAM, String(conversationId))
      window.history.replaceState(null, '', url)
    } catch {
      // 忽略异常环境（如 file://）下的写入失败
    }
  }

  watch(activeConversationId, (conversationId) => {
    showWorkspaceFiles.value = false
    syncConversationUrl(conversationId)
  })

  const activeWorkspaceLabel = computed(() => {
    if (!['codex', 'claude'].includes(activeConversation.value?.agent)) return ''
    const cwd = activeConversation.value.cwd || ''
    return `workspace/${cwd}`
  })
  const activeWorkspaceDisplay = computed(() => {
    const label = activeWorkspaceLabel.value
    if (!label) return ''
    if (activeConversation.value?.workspace_mode === 'parallel') {
      const source = activeConversation.value.source_cwd ?? activeConversation.value.cwd ?? ''
      return `${source.split('/').filter(Boolean).at(-1) || 'workspace'} · 副本`
    }
    const parts = label.split('/').filter(Boolean)
    return parts.at(-1) || label
  })
  return { showWorkspaceFiles, readConversationIdFromUrl,
    activeWorkspaceLabel, activeWorkspaceDisplay }
}
