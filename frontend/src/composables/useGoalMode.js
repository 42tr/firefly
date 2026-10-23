import { computed, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'
import { useRealtimeSubscription } from './useAssistantRealtime.js'

const ACTIVE_STATUSES = new Set(['planning', 'running', 'checking', 'continuing'])

export function useGoalMode({ activeConversationId, onConversationPatched = () => {} } = {}) {
  const goal = ref(null)
  const stages = ref([])
  const isDrawerOpen = ref(false)
  const isLoading = ref(false)
  const isMutating = ref(false)
  const error = ref('')
  // This switch is intentionally per-next-message. A paused goal can be
  // resumed explicitly from the drawer without changing ordinary chat input.
  const enabledForNextMessage = ref(false)

  const active = computed(() => Boolean(goal.value && ACTIVE_STATUSES.has(goal.value.status)))
  const hasGoal = computed(() => Boolean(goal.value))
  const status = computed(() => goal.value?.status || 'idle')
  const currentStage = computed(() => stages.value.find(stage => stage.id === goal.value?.current_stage_id) || stages.value.find(stage => stage.status === 'active') || stages.value[0] || null)
  const completedStages = computed(() => stages.value.filter(stage => stage.status === 'completed').length)
  const progressLabel = computed(() => stages.value.length ? `${completedStages.value} / ${stages.value.length}` : '')

  const apply = (data) => {
    goal.value = data?.goal || null
    stages.value = Array.isArray(data?.stages) ? data.stages : []
    if (!goal.value) enabledForNextMessage.value = false
    else if (goal.value.status === 'paused') enabledForNextMessage.value = false
    return goal.value
  }

  const load = async (conversationId = activeConversationId?.value) => {
    if (!conversationId) {
      goal.value = null
      stages.value = []
      enabledForNextMessage.value = false
      return null
    }
    isLoading.value = true
    error.value = ''
    try {
      const response = await apiFetch(`/api/assistant/conversations/${conversationId}/goal`, { credentials: 'include' })
      if (!response.ok) throw new Error(await response.text() || '加载目标进度失败')
      return apply(await response.json())
    } catch (err) {
      error.value = err.message || '加载目标进度失败'
      return null
    } finally {
      isLoading.value = false
    }
  }

  const create = async (prompt) => {
    const conversationId = activeConversationId?.value
    if (!conversationId || !prompt?.trim()) return null
    isMutating.value = true
    error.value = ''
    try {
      const response = await apiFetch(`/api/assistant/conversations/${conversationId}/goal`, {
        method: 'POST', credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ prompt: prompt.trim() }),
      })
      if (!response.ok) throw new Error(await response.text() || '创建 Goal 失败')
      const next = apply(await response.json())
      enabledForNextMessage.value = Boolean(next)
      return next
    } catch (err) {
      error.value = err.message || '创建 Goal 失败'
      return null
    } finally {
      isMutating.value = false
    }
  }

  const update = async (action) => {
    const conversationId = activeConversationId?.value
    if (!conversationId || !goal.value) return null
    isMutating.value = true
    error.value = ''
    try {
      const response = await apiFetch(`/api/assistant/conversations/${conversationId}/goal`, {
        method: 'PATCH', credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ action }),
      })
      if (!response.ok) throw new Error(await response.text() || '更新 Goal 失败')
      const next = apply(await response.json())
      enabledForNextMessage.value = action === 'resume' && Boolean(next)
      return next
    } catch (err) {
      error.value = err.message || '更新 Goal 失败'
      return null
    } finally {
      isMutating.value = false
    }
  }

  const setEnabled = async (value) => {
    if (value) {
      if (!goal.value || ['completed', 'cancelled'].includes(goal.value.status)) {
        enabledForNextMessage.value = true
        return true
      }
      enabledForNextMessage.value = true
      if (['paused', 'needs_review', 'failed', 'limit_reached'].includes(goal.value.status)) await update('resume')
      return true
    }
    enabledForNextMessage.value = false
    if (goal.value && ACTIVE_STATUSES.has(goal.value.status)) await update('pause')
    return false
  }

  const prepareSubmission = async (prompt) => {
    if (!enabledForNextMessage.value) {
      if (goal.value && ACTIVE_STATUSES.has(goal.value.status)) await update('pause')
      return null
    }
    if (!goal.value || ['completed', 'cancelled'].includes(goal.value.status)) {
      const created = await create(prompt)
      return created?.id || false
    }
    if (['paused', 'needs_review', 'failed', 'limit_reached'].includes(goal.value.status)) {
      const resumed = await update('resume')
      if (!resumed) return false
    }
    return goal.value?.id || false
  }

  const toggleDrawer = () => { isDrawerOpen.value = !isDrawerOpen.value }
  const closeDrawer = () => { isDrawerOpen.value = false }

  watch(activeConversationId, id => {
    enabledForNextMessage.value = false
    if (!id) {
      goal.value = null
      stages.value = []
    }
  })
  useRealtimeSubscription('conversations', () => ({ selected_id: activeConversationId?.value }), () => {
    if (activeConversationId?.value) void load(activeConversationId.value)
  }, { enabled: () => Boolean(activeConversationId?.value) })

  return {
    goal,
    stages,
    status,
    active,
    hasGoal,
    currentStage,
    progressLabel,
    completedStages,
    isDrawerOpen,
    isLoading,
    isMutating,
    error,
    enabledForNextMessage,
    load,
    create,
    update,
    setEnabled,
    prepareSubmission,
    toggleDrawer,
    closeDrawer,
  }
}
