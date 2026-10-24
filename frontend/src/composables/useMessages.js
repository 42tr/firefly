import { ref, nextTick } from 'vue'
import { useMarkdown } from '@/composables/useMarkdown'
import { useEcharts } from './useEcharts'
import { apiFetch } from '@/utils/http'
import { generatedImageOutput } from './generatedImage.js'
import { useMessageScroll } from './useMessageScroll.js'

const welcomeMessage = () => [
  {
    role: 'assistant',
    content: '👋 欢迎使用！想问我什么？',
  },
]

export function useMessages() {
  const messages = ref([])
  const isLoadingMessages = ref(false)
  const { chatPanel, isPinnedToBottom, scrollToBottom, vStableReply } = useMessageScroll()

  const { safeParseJson } = useMarkdown()
  const { renderChart, disposeAllCharts, extractEchartsFromText, extractEchartsFromTool, resolveChartTitle } = useEcharts()

  const normalizeAttachments = (items = []) => {
    if (!Array.isArray(items)) return []
    return items.map((item) => ({
      id: item.id ?? null,
      name: item.file_name || item.name || '附件',
      size: item.file_size ?? item.size ?? 0,
      type: item.content_type || item.type || 'application/octet-stream',
      url: item.id ? `/api/assistant/attachments/${item.id}` : item.url || '',
      previewUrl: item.previewUrl || '',
    }))
  }

  const appendAssistantFromText = (list, text, attachments = [], metadata = null) => {
    const goal = metadata?.goal || null
    if (!text) {
      list.push({ role: 'assistant', content: '', attachments, goal })
      return
    }
    const chartPayload = extractEchartsFromText(text)
    if (chartPayload) {
      const chartId = `chart-${Date.now()}-${Math.random().toString(16).slice(2)}`
      const chartTitle = `📈 ${resolveChartTitle(chartPayload)}`
      list.push({
        role: 'assistant',
        title: chartTitle,
        content: '',
        chartId,
        chartOption: chartPayload.option,
        attachments,
        goal,
      })
    } else {
      list.push({ role: 'assistant', content: text, attachments, goal })
    }
  }


  const appendToolFromRecord = (list, record, attachments = []) => {
    const parsed = safeParseJson(record.content || '')
    const imageOutput = generatedImageOutput({ name: record.metadata?.tool_name, result: parsed, is_error: record.is_error })
    if (imageOutput) {
      list.push({ role: 'assistant', kind: 'image-result', content: '', imageOutput, attachments: [] })
      return
    }
    const payload = parsed ? { result: parsed } : null
    const chartPayload = payload ? extractEchartsFromTool(payload) : null
    if (chartPayload) {
      const chartId = `chart-${Date.now()}-${Math.random().toString(16).slice(2)}`
      const chartTitle = `📈 ${resolveChartTitle(chartPayload, record.metadata?.tool_name)}`
      list.push({
        role: 'assistant',
        title: chartTitle,
        content: '',
        chartId,
        chartOption: chartPayload.option,
        attachments,
      })
      renderChart(chartId, chartPayload.option)
      return
    }
    return false
  }

  const appendRecordFromServer = (list, pendingTools, record) => {
      if (record.role === 'process') {
        list.push({
          role: 'tool', kind: 'process-summary', id: record.id,
          startedAt: record.started_at, completedAt: record.completed_at,
          status: record.status, detailOmitted: true,
          processRange: { start_id: record.start_id, end_id: record.end_id },
        })
        return
      }
      const attachments = normalizeAttachments(record.attachments || [])
      // 过程详情被后端剥离（已完成任务），展开过程块时才按需拉取
      const detailOmitted = record.detail_omitted === true
      if (record.role === 'assistant') {
        const hasToolCalls = Array.isArray(record.tool_calls) && record.tool_calls.length > 0
        if (record.content?.trim()) {
          appendAssistantFromText(list, record.content, attachments, record.metadata)
        }
        if (hasToolCalls) {
          record.tool_calls.forEach((call) => {
            const isHandoff = call.name?.startsWith('handoff_to_')
            const message = isHandoff
              ? {
                  role: 'handoff',
                  kind: 'handoff',
                  id: call.id,
                  agent: call.name.slice('handoff_to_'.length),
                  input: call.arguments ?? null,
                  output: null,
                  status: 'running',
                  tools: (call.steps || []).map(step => ({ ...step, input: null, output: null })),
                  detailOmitted,
                  startedAt: record.created_at || null,
                }
              : {
                  role: 'tool',
                  kind: 'tool-run',
                  id: call.id,
                  name: call.name || '',
                  input: call.arguments ?? null,
                  output: null,
                  status: 'running',
                  attachments: [],
                  detailOmitted,
                  startedAt: record.created_at || null,
                }
            list.push(message)
            if (call.id) pendingTools.set(call.id, message)
          })
        }
      } else if (record.role === 'tool') {
        const toolRun = pendingTools.get(record.tool_call_id)
        const parsed = safeParseJson(record.content || '')
        const output = parsed ?? record.content ?? ''
        const toolName = record.metadata?.tool_name || toolRun?.name || ''
        const handoffAgent = record.metadata?.agent || (toolName.startsWith('handoff_to_') ? toolName.slice('handoff_to_'.length) : '')
        const handoffSteps = Array.isArray(record.metadata?.steps) ? record.metadata.steps : null
        if (toolRun?.kind === 'handoff' && handoffSteps) {
          if (!detailOmitted) toolRun.output = output
          toolRun.detailOmitted = detailOmitted
          toolRun.status = record.is_error || output?.status === 'failed' ? 'error' : 'completed'
          toolRun.completedAt = record.created_at || null
          toolRun.tools = handoffSteps.map((step) => ({
            id: step.id,
            name: step.name || '',
            input: step.input ?? null,
            output: step.output ?? null,
            status: step.status || 'completed',
            attachments: [],
          }))
          pendingTools.delete(record.tool_call_id)
          return
        }
        if (toolRun?.kind === 'handoff') {
          if (!detailOmitted) toolRun.output = output
          toolRun.status = record.is_error ? 'error' : 'completed'
          toolRun.completedAt = record.created_at || null
          toolRun.detailOmitted = toolRun.detailOmitted || detailOmitted
          pendingTools.delete(record.tool_call_id)
          return
        }

        if (handoffAgent && handoffSteps) {
          list.push({
            role: 'handoff',
            kind: 'handoff',
            id: record.tool_call_id,
            agent: handoffAgent,
            input: null,
            output: detailOmitted ? null : output,
            detailOmitted,
            status: record.is_error || output?.status === 'failed' ? 'error' : 'completed',
            startedAt: record.created_at || null,
            completedAt: record.created_at || null,
            tools: handoffSteps.map((step) => ({
              id: step.id,
              name: step.name || '',
              input: step.input ?? null,
              output: step.output ?? null,
              status: step.status || 'completed',
              attachments: [],
            })),
          })
          pendingTools.delete(record.tool_call_id)
          return
        }
        const specialized = appendToolFromRecord(
          list,
          { ...record, metadata: { ...record.metadata, tool_name: toolName } },
          attachments,
        )
        if (specialized !== false) {
          if (toolRun) {
            list.splice(list.indexOf(toolRun), 1)
            pendingTools.delete(record.tool_call_id)
            return 1
          }
          pendingTools.delete(record.tool_call_id)
          return 0
        }
        if (toolRun) {
          if (!detailOmitted) {
            toolRun.input = record.metadata?.tool_input ?? toolRun.input
            toolRun.output = output
            toolRun.attachments = attachments
          }
          toolRun.status = record.is_error ? 'error' : 'completed'
          toolRun.completedAt = record.created_at || null
          toolRun.detailOmitted = toolRun.detailOmitted || detailOmitted
          pendingTools.delete(record.tool_call_id)
        } else {
          list.push({
            role: 'tool',
            kind: 'tool-run',
            id: record.tool_call_id,
            name: toolName,
            input: detailOmitted ? null : record.metadata?.tool_input ?? null,
            output: detailOmitted ? null : output,
            status: record.is_error ? 'error' : 'completed',
            attachments,
            detailOmitted,
            startedAt: record.created_at || null,
            completedAt: record.created_at || null,
          })
        }
      } else {
        list.push({
          role: 'user',
          id: record.id,
          content: record.content || '',
          attachments,
          generationJobId: record.generation_job_id || null,
          processingStatus: record.processing_status || null,
          processingError: record.processing_error || '',
          goal: record.metadata?.goal || null,
        })
      }
  }

  const transformServerMessages = (records = [], { preserveRunning = false } = {}) => {
    const list = []
    const pendingTools = new Map()
    records.forEach((record) => {
      let startIndex = list.length
      // Replacing a pending tool with a result card shifts the new card left.
      startIndex -= appendRecordFromServer(list, pendingTools, record) || 0
      // 同一条记录展开出的所有气泡统一标记来源 job 与时间；
      // 恢复生成（resumeJob 会从 seq 0 重放该 job 的全部事件）前按 jobId 去重，避免内容重复
      const jobId = record.generation_job_id || null
      for (let i = startIndex; i < list.length; i += 1) {
        list[i].messageId = record.id
        list[i].conversationId = record.conversation_id
        if (jobId) list[i].jobId = jobId
        if (!list[i].time) list[i].time = record.created_at || null
        list[i].jobStartedAt = record.processing_started_at || null
        list[i].jobCompletedAt = record.processing_completed_at || null
        list[i].jobStatus = record.processing_status || (record.role === 'process' ? record.status : null)
      }
    })
    const jobStates = new Map(records.filter(record => record.generation_job_id && record.processing_status)
      .map(record => [record.generation_job_id, record]))
    // 没有持久化结果不能推定成功，尤其是取消后恢复权威快照的场景。
    // 活跃任务的快照保留 running；终态按任务状态收尾，并明确执行结果未知。
    for (const toolRun of pendingTools.values()) {
      if (preserveRunning) continue
      const record = jobStates.get(toolRun.jobId)
      const cancelled = record?.processing_status === 'cancelled'
        || (record?.processing_status === 'failed' && record?.processing_error === '用户已停止生成')
      toolRun.status = cancelled ? 'cancelled' : record?.processing_status === 'failed' ? 'error' : 'interrupted'
      toolRun.output = {
        error: 'interrupted_unknown_outcome',
        message: '工具未返回结果，执行结果未知，操作可能已经生效；请检查实际状态，不要直接重复执行。',
      }
      toolRun.completedAt = toolRun.completedAt || toolRun.time || null
    }
    return list
  }

  const loadConversationMessages = async (conversationId, options = {}) => {
    if (!conversationId) return
    isLoadingMessages.value = true
    try {
      const params = new URLSearchParams()
      if (Number.isFinite(options.limit)) params.set('limit', String(options.limit))
      if (Number.isFinite(options.before)) params.set('before', String(options.before))
      const query = params.toString()
      const resp = await apiFetch(
        `/api/assistant/conversations/${conversationId}/messages${query ? `?${query}` : ''}`,
        { credentials: 'include' },
      )
      if (!resp.ok) {
        const text = await resp.text()
        throw new Error(text || '获取对话失败')
      }
      const data = await resp.json()
      return {
        messages: data.messages || [],
        hasMore: data.has_more === true,
        nextBefore: Number.isFinite(data.next_before) ? data.next_before : null,
        sourceReply: data.source_reply || null,
      }
    } finally {
      isLoadingMessages.value = false
    }
  }

  const renderCharts = async () => {
    await nextTick()
    messages.value.forEach((msg) => {
      if (msg.chartOption && msg.chartId) {
        renderChart(msg.chartId, msg.chartOption)
      }
    })
  }

  return {
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
  }
}
