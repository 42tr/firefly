import { computed, getCurrentInstance, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { useEcharts } from './useEcharts.js'
import { generatedImageOutput } from './generatedImage.js'
import { apiFetch } from '@/utils/http'
import { newSubmissionId } from './submissionIdentity'
import { useAssistantRealtime, useRealtimeSubscription } from './useAssistantRealtime.js'

const MAX_RECONNECT_ATTEMPTS = 10
const RECONNECT_BASE_MS = 1000
const RECONNECT_MAX_MS = 30000

// Submission acknowledgements must not move a known job back into the queue.
const processingProgress = { submitting: 0, queued: 1, running: 2, cancelling: 3, completed: 4, failed: 4, cancelled: 4 }
const advanceProcessingState = (message, state) => {
  if (!state?.status || (processingProgress[state.status] ?? -1) < (processingProgress[message.processingStatus] ?? -1)) return
  message.processingStatus = state.status === 'cancelled' ? 'failed' : state.status
  message.processingError = state.status === 'cancelled' ? '用户已停止生成' : state.error || ''
}

const getTimestamp = (value) => {
  if (!value) return null
  const timestamp = new Date(value).getTime()
  return Number.isFinite(timestamp) ? timestamp : null
}

export const getJobStartedAt = (messages, jobId) => {
  if (!jobId) return null
  const jobMessages = (messages || [])
    .filter((message) => message.jobId === jobId || message.generationJobId === jobId)
  const recordedStart = jobMessages.map(message => getTimestamp(message.jobStartedAt)).find(time => time !== null)
  if (recordedStart !== undefined) return new Date(recordedStart).toISOString()
  const processMessages = jobMessages.filter((message) => message.role !== 'user')
  const timestamps = (processMessages.length ? processMessages : jobMessages)
    .map((message) => getTimestamp(message.startedAt || message.time))
    .filter((timestamp) => timestamp !== null)
  return timestamps.length ? new Date(Math.min(...timestamps)).toISOString() : null
}

export function useChatStream({
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
  ensureConversation,
  canSend = () => true,
  onModuleToolChanged = () => {},
  transformServerMessages = records => records,
  onMessagesRestored = () => {},
  followQueue = true,
}) {
  const input = ref('')
  const localSubmissions = new Map()
  // A stream can start or finish before the POST response identifies its user
  // message. Retain those observations until outstanding submissions settle.
  const pendingJobStates = new Map()
  const getLocalSubmissions = (id) => [...(localSubmissions.get(id) || [])]
  const disposeLocalSubmissions = () => {
    for (const pending of localSubmissions.values()) {
      for (const message of pending) {
        for (const file of message.retryAttachments || []) if (file.previewUrl) URL.revokeObjectURL(file.previewUrl)
      }
    }
    localSubmissions.clear()
    pendingJobStates.clear()
  }
  const queuedMessages = computed(() => messages.value.filter(
    (message) => message.role === 'user'
      && ['submitting', 'queued'].includes(message.processingStatus),
  ))
  const { extractEchartsFromTool, extractEchartsFromText, resolveChartTitle, renderChart } = useEcharts()

  let activeStream = null
  let currentJobId = null
  let executionId = null
  let lastEventSeq = 0
  let reconnectAttempts = 0
  let reconnectTimer = null
  let isReconnecting = false
  let streamDoneReceived = false
  let streamGeneration = 0
  const realtime = useAssistantRealtime()
  let activeGenerationSubscription = null
  let recoverySubscription = null
  const stopRecovery = () => { recoverySubscription?.stop(); recoverySubscription = null }
  let replayStartedAt = null
  let currentJobStartedAt = null
  let currentEventTime = null
  const setJobTiming = (jobId, { startedAt, completedAt, status, error }) => {
    if (!jobId) return
    if (status && localSubmissions.size) {
      pendingJobStates.set(jobId, { ...pendingJobStates.get(jobId), status,
        ...(startedAt ? { startedAt } : {}),
        error: error ?? (status === 'running' ? '' : pendingJobStates.get(jobId)?.error || '') })
    }
    for (const message of messages.value) {
      if (message.jobId !== jobId && message.generationJobId !== jobId) continue
      if (startedAt) message.jobStartedAt = startedAt
      if (completedAt) message.jobCompletedAt = completedAt
      if (status) message.jobStatus = status
      if (message.role === 'user' && status === 'running') advanceProcessingState(message, { status })
    }
  }
  const removeQueuedMessage = async (message) => {
    if (!message?.generationJobId) return
    try {
      const response = await apiFetch(`/api/assistant/jobs/${message.generationJobId}`, {
        method: 'DELETE',
        credentials: 'include',
      })
      if (!response.ok) throw new Error((await response.text()) || '删除排队消息失败')
      const index = messages.value.indexOf(message)
      if (index >= 0) messages.value.splice(index, 1)
      fetchConversations({ silent: true })
    } catch (err) {
      conversationError.value = err.message || '删除排队消息失败'
    }
  }

  const isCurrentContext = (context) => context && context.generation === streamGeneration


  const findActiveHandoff = (agent) => {
    for (let index = messages.value.length - 1; index >= 0; index -= 1) {
      const message = messages.value[index]
      if (message.kind === 'handoff' && message.agent === agent && message.status === 'running') return message
    }
    return null
  }

  const findHandoff = (id, agent) =>
    messages.value.find(
      (message) =>
        message.kind === 'handoff' &&
        (message.id === id || (agent && message.agent === agent && message.status === 'running')),
    )

  const findToolRun = (items, id) => items?.find((item) => item.id === id)

  // 流式期间新 push 的气泡打上当前 jobId，resumeJob 重放时按 jobId 去重才有据可依，
  // 不再依赖「resume 前必经历史重载」这一隐式约定
  const pushMessage = (msg) => {
    if (msg.role !== 'user' && currentJobId && msg.jobId === undefined) {
      msg.jobId = currentJobId
    }
    if (!msg.time) msg.time = currentEventTime || replayStartedAt || new Date().toISOString()
    if (msg.jobId === currentJobId) {
      if (currentJobStartedAt) msg.jobStartedAt = currentJobStartedAt
      msg.jobStatus = 'running'
    }
    if ((msg.kind === 'tool-run' || msg.kind === 'handoff') && !msg.startedAt) msg.startedAt = msg.time
    replayStartedAt = null
    messages.value.push(msg)
  }

  const removeToolRun = (id) => {
    const index = messages.value.findIndex((item) => item.kind === 'tool-run' && item.id === id)
    if (index >= 0) messages.value.splice(index, 1)
  }

  const resetStreamState = () => {
    stopRecovery()
    activeStream = null
    currentJobId = null
    executionId = null
    lastEventSeq = 0
    reconnectAttempts = 0
    isReconnecting = false
    streamDoneReceived = false
    replayStartedAt = null
    currentJobStartedAt = null
    currentEventTime = null
    if (reconnectTimer) {
      clearTimeout(reconnectTimer)
      reconnectTimer = null
    }

  }

  const abortStream = () => {
    streamGeneration += 1
    if (activeStream) {
      activeStream.abort()
      activeStream = null
    }
    resetStreamState()
    isStreaming.value = false
  }

  const cancelJob = async (jobId) => {
    if (!jobId) return false
    try {
      const response = await apiFetch(`/api/assistant/jobs/${jobId}`, {
        method: 'DELETE',
        credentials: 'include',
      })
      return response.ok
    } catch (err) {
      console.error('Failed to cancel generation', err)
      return false
    }
  }

  const stopGeneration = async () => {
    const jobId = currentJobId
    const conversationId = activeConversationId.value
    const stoppedAt = new Date().toISOString()
    setJobTiming(jobId, { completedAt: stoppedAt, status: 'cancelled' })

    // Stop rendering immediately. The API call below stops the server-side
    // generation as well, so this job cannot be resumed after disconnecting.
    abortStream()
    messages.value.forEach((message) => {
      if (message.status === 'running') {
        message.status = 'cancelled'
        message.completedAt = stoppedAt
      }
      message.tools?.forEach((tool) => {
        if (tool.status === 'running') {
          tool.status = 'cancelled'
          tool.completedAt = stoppedAt
        }
      })
    })
    let cancelled = await cancelJob(jobId)
    let cancelledJobId = cancelled ? jobId : null

    // The user can press stop before the initial SSE event yields its job id.
    // Look up the active job once to cover that small race window.
    if (!cancelled && conversationId) {
      try {
        const response = await apiFetch(`/api/assistant/conversations/${conversationId}/active-generation`, {
          credentials: 'include',
        })
        if (response.ok) {
          const data = await response.json()
          if (data.active && data.job_id) {
            cancelled = await cancelJob(data.job_id)
            if (cancelled) cancelledJobId = data.job_id
          }
        }
      } catch (err) {
        console.error('Failed to find active generation for cancellation', err)
      }
    }
    if (conversationId) {
      if (cancelled) {
        const currentUserMessage = cancelledJobId
          ? messages.value.find((message) => message.generationJobId === cancelledJobId)
          : null
        if (currentUserMessage) {
          currentUserMessage.processingStatus = 'failed'
          currentUserMessage.processingError = '用户已停止生成'
        }
        resumeNextQueued(conversationId, jobId)
      } else {
        fetchConversations({ silent: true })
      }
    }
  }

  const clearReconnect = () => {
    if (reconnectTimer) {
      clearTimeout(reconnectTimer)
      reconnectTimer = null
    }
    isReconnecting = false
    reconnectAttempts = 0
  }

  const resumeNextQueued = async (conversationId, previousJobId = null) => {
    if (!followQueue || !conversationId || conversationId !== activeConversationId.value) return
    if (realtime) { activeGenerationSubscription?.refresh(); return }
    try {
      const response = await apiFetch(`/api/assistant/conversations/${conversationId}/active-generation`, {
        credentials: 'include',
      })
      if (response.ok) {
        const data = await response.json()
        // Cancellation keeps its queue slot until the worker exits. It must
        // not restart a stopped stream or mark this conversation completed.
        if (data.active && data.status === 'cancelling') {
          fetchConversations({ silent: true })
          return
        }
        if (data.active && data.job_id && data.job_id !== previousJobId) {
          const nextUserMessage = messages.value.find((message) => message.generationJobId === data.job_id)
          if (nextUserMessage) nextUserMessage.processingStatus = 'running'
          resumeJob(data.job_id, conversationId)
          return
        }
      }
    } catch (err) {
      console.error('Failed to continue queued generation', err)
    }
    patchConversation(conversationId, { status: 'completed' })
    fetchConversations({ silent: true })
  }

  // An expired replay log needs one committed snapshot, then socket updates.
  // Never poll the full process while waiting for the job to finish.
  const recoverFromSnapshot = async (context) => {
    if (!isCurrentContext(context) || !currentJobId) return
    const jobId = currentJobId
    activeStream?.abort()
    stopRecovery()
    clearReconnect()
    lastEventSeq = 0
    executionId = null
    const recovery = { ...context, generation: ++streamGeneration }
    const controller = new AbortController()
    activeStream = controller
    const restore = (data) => {
      if (!isCurrentContext(recovery) || recovery.conversationId !== activeConversationId.value) return
      const records = data.messages || []
      const status = records.find(record => record.role === 'user')?.processing_status
        || records.find(record => record.processing_status)?.processing_status
      setJobTiming(jobId, { status, error: records.find(record => record.processing_error)?.processing_error })
      const restored = transformServerMessages(records, { preserveRunning: ['running', 'queued', 'cancelling'].includes(status) })
      const belongsToJob = message => message.jobId === jobId || message.generationJobId === jobId
      const index = messages.value.findIndex(belongsToJob)
      const remaining = messages.value.filter(message => !belongsToJob(message))
      remaining.splice(index < 0 ? remaining.length : index, 0, ...restored)
      messages.value = remaining
      conversationError.value = ''
      onMessagesRestored()
      scrollToBottom()
      if (!records.length || ['completed', 'failed', 'cancelled'].includes(status)) {
        stopRecovery()
        streamDoneReceived = true
        isStreaming.value = false
        fetchConversations({ silent: true })
        resumeNextQueued(recovery.conversationId, jobId)
      }
    }
    try {
      const response = await apiFetch(`/api/assistant/conversations/${recovery.conversationId}/process?job_id=${encodeURIComponent(jobId)}`, {
        credentials: 'include', signal: controller.signal,
      })
      if (!response.ok) {
        throw Object.assign(new Error(response.status === 404 ? '对话或任务已不存在' : '恢复已保存的回复失败'),
          { terminal: response.status >= 400 && response.status < 500 })
      }
      restore(await response.json())
    } catch (err) {
      if (!isCurrentContext(recovery) || err.name === 'AbortError') return
      conversationError.value = err.message || '恢复已保存的回复失败，请重新打开对话'
      if (err.terminal) { isStreaming.value = false; return }
    }
    if (!isCurrentContext(recovery) || recovery.conversationId !== activeConversationId.value || streamDoneReceived) return
    if (realtime) {
      const subscription = realtime.subscribe('process', { conversation_id: recovery.conversationId, job_id: jobId }, restore,
        message => { if (isCurrentContext(recovery)) conversationError.value = message })
      // A shared subscription may synchronously deliver a cached terminal snapshot.
      if (isCurrentContext(recovery) && !streamDoneReceived) recoverySubscription = subscription
      else subscription.stop()
    } else {
      isStreaming.value = false
      conversationError.value ||= '实时连接不可用，请重新打开对话以同步执行结果'
    }
  }

  const handleEvent = async (rawEvent, context) => {
    if (!isCurrentContext(context)) return
    if (!rawEvent.trim()) return
    const lines = rawEvent.split(/\r?\n/)
    let eventType = 'message'
    const dataLines = []

    for (const line of lines) {
      if (line.startsWith('event:')) {
        eventType = line.slice(6).trim()
      } else if (line.startsWith('data:')) {
        dataLines.push(line.slice(5).trim())
      }
    }

    if (!dataLines.length) return
    const dataText = dataLines.join('\n')
    if (!dataText) return

    let payload = null
    try {
      payload = JSON.parse(dataText)
    } catch (err) {
      console.error('Failed to parse payload', err, dataText)
      return
    }

    if (eventType === 'stream_reset' || (eventType === 'stream_started' && payload.snapshot_required)) {
      await recoverFromSnapshot(context)
      return
    }

    if (payload.execution_id) {
      if (executionId && executionId !== payload.execution_id) {
        await recoverFromSnapshot(context)
        return
      }
      executionId = payload.execution_id
    }

    if (eventType === 'job_started' || eventType === 'stream_started') {
      if (payload.job_id) {
        currentJobId = payload.job_id
      }
      if (getTimestamp(payload.started_at) !== null) currentJobStartedAt = payload.started_at
      setJobTiming(currentJobId, { startedAt: currentJobStartedAt, status: payload.queued ? 'queued' : 'running' })
      return
    }

    // seq 去重/断点续传
    const seq = typeof payload.seq === 'number' ? payload.seq : null
    if (seq !== null) {
      if (seq <= lastEventSeq) {
        return
      }
      lastEventSeq = seq
    }

    const streamingConversationId = context.conversationId
    if (streamingConversationId !== activeConversationId.value) {
      return
    }
    // Replayed events retain their server time instead of becoming new browser-time events.
    currentEventTime = getTimestamp(payload.occurred_at) !== null ? payload.occurred_at : null
    const eventTime = currentEventTime || new Date().toISOString()

    if (eventType === 'handoff_started') {
      context.activeAssistantMessage = null
      const existing = findHandoff(payload.id, payload.agent)
      if (existing) {
        existing.id = payload.id || existing.id
        existing.agent = payload.agent || existing.agent
        existing.status = 'running'
      } else {
        pushMessage({
          role: 'handoff',
          kind: 'handoff',
          id: payload.id,
          agent: payload.agent || 'agent',
          input: null,
          output: null,
          status: 'running',
          tools: [],
        })
      }
    } else if (eventType === 'assistant_delta') {
      const delta = payload.delta || ''
      if (delta) {
        if (context.activeAssistantMessage === null) {
          pushMessage({ role: 'assistant', content: '' })
          context.activeAssistantMessage = messages.value.at(-1)
        }
        context.activeAssistantMessage.content += delta
        context.activeAssistantMessage.completedAt = eventTime
      }
    } else if (eventType === 'tool_call') {
      context.activeAssistantMessage = null
      if (payload.name?.startsWith('handoff_to_')) {
        const agent = payload.name.slice('handoff_to_'.length)
        const existing = findHandoff(payload.id, agent)
        if (existing) {
          existing.input = payload.arguments ?? null
        } else {
          pushMessage({
            role: 'handoff',
            kind: 'handoff',
            id: payload.id,
            agent,
            input: payload.arguments ?? null,
            output: null,
            status: 'running',
            tools: [],
          })
        }
        scrollToBottom()
        return
      }
      if (payload.agent) {
        const handoff = findActiveHandoff(payload.agent)
        if (handoff) {
          handoff.tools.push({
            id: payload.id,
            name: payload.name || '',
            input: payload.arguments ?? null,
            output: null,
            status: 'running',
            attachments: [],
            startedAt: eventTime,
          })
          scrollToBottom()
          return
        }
      }
      pushMessage({
        role: 'tool',
        kind: 'tool-run',
        id: payload.id,
        name: payload.name || '',
        input: payload.arguments ?? null,
        output: null,
        status: 'running',
        attachments: [],
      })
    } else if (eventType === 'tool_result') {
      context.activeAssistantMessage = null
      const toolName = payload.name || ''
      if (toolName.startsWith('mcp__') && !payload.is_error && !payload.error) onModuleToolChanged()
      const attachments = normalizeAttachments(payload.attachments || [])
      if (toolName.startsWith('handoff_to_')) {
        const agent = payload.result?.agent || toolName.slice('handoff_to_'.length)
        const handoff = findHandoff(payload.id, agent) || findActiveHandoff(agent)
        if (handoff) {
          handoff.status = payload.is_error || payload.error ? 'error' : 'completed'
          handoff.output = payload.result
          handoff.completedAt = eventTime
          scrollToBottom()
          return
        }
      }
      if (payload.agent) {
        const handoff = findActiveHandoff(payload.agent)
        const toolRun = findToolRun(handoff?.tools, payload.id)
        if (toolRun) {
          toolRun.output = payload.result ?? payload.error ?? null
          toolRun.status = payload.is_error || payload.error ? 'error' : 'completed'
          toolRun.attachments = attachments
          toolRun.completedAt = eventTime
          scrollToBottom()
          return
        }
      }

      const chartPayload = extractEchartsFromTool({ ...payload, result: payload.presentation ?? payload.result })
      const imageOutput = generatedImageOutput({ ...payload, result: payload.presentation ?? payload.result })
      if (imageOutput) {
        removeToolRun(payload.id)
        pushMessage({ role: 'assistant', kind: 'image-result', content: '', imageOutput, attachments: [],
          messageId: payload.message_id ?? null })
      } else if (chartPayload) {
        removeToolRun(payload.id)
        const chartId = `chart-${Date.now()}-${Math.random().toString(16).slice(2)}`
        const chartTitle = `📈 ${resolveChartTitle(chartPayload, payload.name)}`
        pushMessage({
          role: 'assistant',
          title: chartTitle,
          content: '',
          chartId,
          chartOption: chartPayload.option,
          attachments,
        })
        renderChart(chartId, chartPayload.option)
      } else {
        const toolRun = findToolRun(messages.value, payload.id)
        if (toolRun?.kind === 'tool-run') {
          toolRun.input = payload.arguments ?? toolRun.input
          toolRun.output = payload.result ?? payload.error ?? null
          toolRun.status = payload.error || payload.is_error ? 'error' : 'completed'
          toolRun.attachments = attachments
          toolRun.completedAt = eventTime
        } else {
          pushMessage({
            role: 'tool',
            kind: 'tool-run',
            id: payload.id,
            name: toolName,
            input: payload.arguments ?? null,
            output: payload.result ?? payload.error ?? null,
            status: payload.error || payload.is_error ? 'error' : 'completed',
            attachments,
            startedAt: eventTime,
            completedAt: eventTime,
          })
        }
      }
    } else if (eventType === 'workspace_progress') {
      patchConversation(payload.conversation_id || streamingConversationId, {
        workspace_state: payload.workspace_state,
        workspace_error: payload.workspace_error || null,
        workspace_progress: payload.message || '',
        ...(payload.cwd ? { cwd: payload.cwd } : {}),
      })
    } else if (eventType === 'conversation_title') {
      const eventConversationId = payload.conversation_id || payload.id
      const nextTitle = typeof payload.title === 'string' ? payload.title.trim() : ''
      if (eventConversationId && nextTitle) {
        const patched = patchConversation(eventConversationId, { title: nextTitle })
        if (!patched) {
          fetchConversations()
        }
        if (eventConversationId === streamingConversationId) {
          context.updatedTitleInStream = true
        }
      }
    } else if (eventType === 'assistant_done') {
      const finishedJobId = currentJobId
      const lastMessage = finishedJobId && [...messages.value].reverse().find(message => message.jobId === finishedJobId
        || message.generationJobId === finishedJobId)
      if (lastMessage && Number.isSafeInteger(payload.last_message_id)) {
        lastMessage.readMessageId = payload.last_message_id
      }
      streamDoneReceived = true
      clearReconnect()
      // 兜底：生成已结束，仍停留在 running 的步骤（工具结果事件丢失等）统一收尾，
      // 否则执行过程卡片会一直显示“正在执行”且耗时持续增长
      const finishedAt = eventTime
      const failed = messages.value.some(message => (message.jobId === finishedJobId || message.generationJobId === finishedJobId)
        && message.jobStatus === 'failed')
      setJobTiming(finishedJobId, { completedAt: finishedAt, status: payload.cancelled ? 'cancelled' : failed ? 'failed' : 'completed' })
      const fallbackStatus = payload.cancelled ? 'cancelled' : 'interrupted'
      messages.value.forEach((message) => {
        if (message.status === 'running') {
          message.status = fallbackStatus
          message.completedAt = message.completedAt || finishedAt
        }
        message.tools?.forEach((tool) => {
          if (tool.status === 'running') {
            tool.status = fallbackStatus
            tool.completedAt = tool.completedAt || finishedAt
          }
        })
      })
      if (context.activeAssistantMessage !== null) {
        const assistantMessage = context.activeAssistantMessage
        const assistantIndex = messages.value.indexOf(assistantMessage)
        const chartPayload = extractEchartsFromText(assistantMessage?.content)
        if (chartPayload && assistantIndex >= 0) {
          const chartId = `chart-${Date.now()}-${Math.random().toString(16).slice(2)}`
          const chartTitle = `📈 ${resolveChartTitle(chartPayload)}`
          messages.value.splice(assistantIndex, 1, {
            ...assistantMessage,
            role: 'assistant',
            title: chartTitle,
            content: '',
            chartId,
            chartOption: chartPayload.option,
          })
          renderChart(chartId, chartPayload.option)
        }
      }
      isStreaming.value = false
      // finishedJobId 可能为 null（job_started 尚未到达），此时不能按
      // generationJobId === null 查找——历史里未关联 job 的消息会被误伤
      const currentUserMessage = finishedJobId
        ? messages.value.find((message) => message.generationJobId === finishedJobId)
        : null
      if (currentUserMessage && currentUserMessage.processingStatus !== 'failed') {
        currentUserMessage.processingStatus = payload.cancelled ? 'failed' : 'completed'
        if (payload.cancelled) currentUserMessage.processingError = '用户已停止生成'
      }
      context.activeAssistantMessage = null
      fetchConversations()
      resumeNextQueued(streamingConversationId, finishedJobId)
    } else if (eventType === 'error') {
      streamDoneReceived = true
      clearReconnect()
      context.activeAssistantMessage = null
      const failedAt = eventTime
      messages.value.forEach((message) => {
        if (message.status === 'running') {
          message.status = 'error'
          message.completedAt = failedAt
        }
        message.tools?.forEach((tool) => {
          if (tool.status === 'running') {
            tool.status = 'error'
            tool.completedAt = failedAt
          }
        })
      })
      pushMessage({
        role: 'assistant',
        content: `❌ ${payload.message || '发生错误'}`,
      })
      setJobTiming(currentJobId, { completedAt: failedAt, status: 'failed', error: payload.message || '生成失败' })
      const currentUserMessage = currentJobId
        ? messages.value.find((message) => message.generationJobId === currentJobId)
        : null
      if (currentUserMessage) {
        currentUserMessage.processingStatus = 'failed'
        currentUserMessage.processingError = payload.message || '生成失败'
      }
      isStreaming.value = false
      resumeNextQueued(streamingConversationId, currentJobId)
    }

    scrollToBottom()
  }

  const consumeStream = async (reader, context) => {
    const decoder = new TextDecoder('utf-8')
    let buffer = ''

    try {
      while (true) {
        const { value, done } = await reader.read()
        if (done) break
        buffer += decoder.decode(value, { stream: true })

        let splitIndex = buffer.indexOf('\n\n')
        while (splitIndex !== -1) {
          const rawEvent = buffer.slice(0, splitIndex)
          buffer = buffer.slice(splitIndex + 2)
          await handleEvent(rawEvent, context)
          if (!isCurrentContext(context)) {
            await reader.cancel()
            return
          }
          splitIndex = buffer.indexOf('\n\n')
        }
      }

      // 流正常结束
      if (isCurrentContext(context) && !streamDoneReceived && currentJobId && context.conversationId === activeConversationId.value) {
        // 服务端关闭但还没收到 assistant_done，可能是短暂断开，触发重连
        scheduleReconnect(currentJobId, context)
      }
    } catch (err) {
      if (err.name === 'AbortError') {
        // 用户主动取消
        if (isCurrentContext(context)) {
          isStreaming.value = false
          resetStreamState()
        }
        return
      }

      console.error('Stream error', err)
      if (isCurrentContext(context) && currentJobId && !streamDoneReceived) {
        scheduleReconnect(currentJobId, context)
      } else if (isCurrentContext(context)) {
        pushMessage({
          role: 'assistant',
          content: `❌ 请求失败：${err.message || err}`,
        })
        isStreaming.value = false
        resetStreamState()
        scrollToBottom()
      }
    }
  }

  const scheduleReconnect = (jobId, context) => {
    if (!isCurrentContext(context) || isReconnecting || streamDoneReceived) return
    if (reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
      recoverFromSnapshot(context)
      return
    }

    isReconnecting = true
    reconnectAttempts += 1
    const delay = Math.min(RECONNECT_BASE_MS * 2 ** (reconnectAttempts - 1), RECONNECT_MAX_MS)

    reconnectTimer = setTimeout(() => {
      reconnectTimer = null
      reconnectStream(jobId, context)
    }, delay)
  }

  const reconnectStream = async (jobId, context) => {
    if (!isCurrentContext(context) || !jobId || streamDoneReceived) return
    isReconnecting = false

    const controller = new AbortController()
    activeStream = controller

    try {
      const query = new URLSearchParams({ after_seq: String(lastEventSeq) })
      if (executionId) query.set('execution_id', executionId)
      const resp = await apiFetch(`/api/assistant/jobs/${jobId}/stream?${query}`, {
        method: 'GET',
        signal: controller.signal,
        credentials: 'include',
      })

      if (resp.status === 404 || resp.status === 410) {
        await recoverFromSnapshot(context)
        return
      }
      if (!resp.ok || !resp.body) {
        throw new Error('重连失败')
      }

      const reader = resp.body.getReader()
      await consumeStream(reader, context)
    } catch (err) {
      if (err.name === 'AbortError') {
        // 用户主动取消/切换对话，不再重连
        return
      }
      console.error('Reconnect failed', err)
      if (!streamDoneReceived) {
        scheduleReconnect(jobId, context)
      }
    }
  }

  const resumeJob = async (jobId, conversationId) => {
    if (!jobId || !conversationId) return
    stopRecovery()
    if (activeStream) {
      activeStream.abort()
      activeStream = null
    }
    // 重放会从 seq 0 重新经历该 job 的全部事件；历史中该 job 已持久化的
    // 非 user 气泡（assistant 文本/工具/图表/账单）先移除，由重放重建，避免重复。
    replayStartedAt = getJobStartedAt(messages.value, jobId)
    currentJobStartedAt = replayStartedAt
    currentEventTime = null
    messages.value = messages.value.filter((item) => item.role === 'user' || item.jobId !== jobId)
    currentJobId = jobId
    setJobTiming(jobId, { status: 'running' })
    executionId = null
    streamGeneration += 1
    lastEventSeq = 0
    reconnectAttempts = 0
    streamDoneReceived = false
    clearReconnect()
    isStreaming.value = true
    patchConversation(conversationId, { status: 'running' })

    const context = {
      conversationId,
      generation: streamGeneration,
      activeAssistantMessage: null,
      updatedTitleInStream: false,
    }

    const controller = new AbortController()
    activeStream = controller

    try {
      const resp = await apiFetch(`/api/assistant/jobs/${jobId}/stream?after_seq=0`, {
        method: 'GET',
        signal: controller.signal,
        credentials: 'include',
      })

      if (resp.status === 404 || resp.status === 410) {
        await recoverFromSnapshot(context)
        return
      }
      if (!resp.ok || !resp.body) {
        throw new Error('恢复生成失败')
      }

      const reader = resp.body.getReader()
      await consumeStream(reader, context)
    } catch (err) {
      console.error('Resume failed', err)
      if (!streamDoneReceived) {
        scheduleReconnect(jobId, context)
      }
    }
  }

  const submitMessage = async (userText, stagedAttachments, retryMessage = null) => {

    const conversationId = activeConversationId.value
    if (!conversationId) {
      conversationError.value = '请先创建或选择一个对话'
      return false
    }

    const attachmentsForMessage = stagedAttachments.map((item) => {
      return {
        id: item.id,
        name: item.name,
        size: item.size,
        type: item.type,
        previewUrl: item.previewUrl,
        url: '',
      }
    })

    const optimisticMessage = retryMessage || reactive({
      role: 'user',
      content: userText,
      attachments: attachmentsForMessage,
      processingStatus: 'submitting',
      processingError: '',
      generationJobId: null,
      time: new Date().toISOString(),
    })
    optimisticMessage.processingStatus = 'submitting'
    optimisticMessage.processingError = ''
    optimisticMessage.retryAttachments = stagedAttachments
    optimisticMessage.conversationId = conversationId
    // A retry represents the same submission even if its response was lost.
    optimisticMessage.requestId ||= newSubmissionId()
    if (!retryMessage) {
      messages.value.push(optimisticMessage)
      const pending = localSubmissions.get(conversationId) || []
      pending.push(optimisticMessage)
      localSubmissions.set(conversationId, pending)
    }
    await scrollToBottom({ force: true })
    patchConversation(conversationId, { status: 'running' })

    try {
      const form = new FormData()
      form.append('message', userText)
      form.append('enable_tools', 'true')
      stagedAttachments.forEach((item) => form.append('attachments[]', item.file, item.name))

      const resp = await apiFetch(`/api/assistant/conversations/${conversationId}/messages`, {
        method: 'POST',
        body: form,
        headers: { 'Idempotency-Key': optimisticMessage.requestId },
        credentials: 'include',
      })
      if (!resp.ok) {
        const text = await resp.text()
        throw new Error(text || '请求失败')
      }
      const data = await resp.json()
      optimisticMessage.id = data.message?.id || optimisticMessage.id
      optimisticMessage.generationJobId = data.job_id || null
      advanceProcessingState(optimisticMessage, { status: data.status || 'queued', error: data.message?.processing_error })
      if (Array.isArray(data.message?.attachments)) {
        optimisticMessage.attachments = normalizeAttachments(data.message.attachments)
      }
      let existing
      if (activeConversationId.value === conversationId) {
        // History may already contain a committed message whose response was
        // lost. Once its identity is known, keep the history bubble in place.
        existing = messages.value.find(message => message !== optimisticMessage && message.role === 'user'
          && ((optimisticMessage.id != null && (message.id === optimisticMessage.id || message.messageId === optimisticMessage.id))
            || (optimisticMessage.generationJobId && message.generationJobId === optimisticMessage.generationJobId)))
        if (existing) advanceProcessingState(optimisticMessage, {
          status: existing.processingStatus, error: existing.processingError,
        })
      }
      advanceProcessingState(optimisticMessage, pendingJobStates.get(optimisticMessage.generationJobId))
      // A stream can start before the acknowledgement links this bubble to its
      // job. Preserve that server time even if the job has already finished.
      const startedAt = data.message?.processing_started_at || pendingJobStates.get(optimisticMessage.generationJobId)?.startedAt
      if (startedAt) (existing || optimisticMessage).jobStartedAt = startedAt
      if (existing) {
        advanceProcessingState(existing, { status: optimisticMessage.processingStatus, error: optimisticMessage.processingError })
        messages.value = messages.value.filter(message => message !== optimisticMessage)
      }
      for (const item of stagedAttachments) {
        if (!item.previewUrl) continue
        if (activeConversationId.value === conversationId) messageAttachmentUrls.add(item.previewUrl)
        else URL.revokeObjectURL(item.previewUrl)
      }
      delete optimisticMessage.retryAttachments
      const pending = getLocalSubmissions(conversationId).filter(item => item !== optimisticMessage)
      if (pending.length) localSubmissions.set(conversationId, pending)
      else localSubmissions.delete(conversationId)
      if (!localSubmissions.size) pendingJobStates.clear()
      fetchConversations({ silent: true })
      if (activeConversationId.value === conversationId && canSend() && !isStreaming.value
        && optimisticMessage.processingStatus === 'running' && data.job_id) resumeJob(data.job_id, conversationId)
      return true
    } catch (err) {
      optimisticMessage.processingStatus = 'failed'
      optimisticMessage.processingError = err.message || String(err)
      if (activeConversationId.value === conversationId) scrollToBottom()
      return false
    }
  }

  let preparingConversation = false
  const sendMessage = async () => {
    if (!canSend()) return

    const userText = input.value.trim()
    const hasAttachments = chatAttachments.value.length > 0

    if (!userText && !hasAttachments) return

    // Create before clearing the composer so a failed first send retains its
    // text/files. Navigation invalidates this attempt without posting into a
    // different conversation or replacing its draft.
    if (!activeConversationId.value && ensureConversation) {
      if (preparingConversation) return
      const generation = streamGeneration
      preparingConversation = true
      try {
        const created = await ensureConversation()
        if (generation !== streamGeneration || !created?.id || activeConversationId.value !== created.id) return
      } catch (err) {
        if (generation === streamGeneration) conversationError.value = err.message || '创建对话失败'
        return
      } finally {
        preparingConversation = false
      }
    }

    const stagedAttachments = chatAttachments.value.map((item) => ({ ...item }))
    input.value = ''
    clearAttachments({ revokePreview: false })
    await submitMessage(userText, stagedAttachments)
  }

  const retryMessage = async (message) => {
    if (!canSend() || message.processingStatus !== 'failed'
      || message.conversationId !== activeConversationId.value || !message.retryAttachments) return
    await submitMessage(message.content, message.retryAttachments, message)
  }

  if (realtime && followQueue) {
    activeGenerationSubscription = useRealtimeSubscription('active_generation',
      () => ({ conversation_id: activeConversationId.value }), data => {
        if (!data.active || !data.job_id || data.status === 'cancelling' || isStreaming.value || !canSend()) return
        if (data.job_id === currentJobId && streamDoneReceived) return
        const nextUserMessage = messages.value.find(message => message.generationJobId === data.job_id)
        if (nextUserMessage) nextUserMessage.processingStatus = 'running'
        resumeJob(data.job_id, activeConversationId.value)
      }, { enabled: () => !!activeConversationId.value && canSend() })
  }

  if (getCurrentInstance()) {
    watch(activeConversationId, () => { if (recoverySubscription) abortStream() }, { flush: 'sync' })
    onBeforeUnmount(abortStream)
  }

  return {
    input,
    queuedMessages,
    removeQueuedMessage,
    sendMessage,
    retryMessage,
    getLocalSubmissions,
    disposeLocalSubmissions,
    abortStream,
    stopGeneration,
    resumeJob,
  }
}
