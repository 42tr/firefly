import { computed, getCurrentInstance, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { orderMessagesByTurn } from './messageOrder.js'
import { apiFetch } from '@/utils/http'
import { useAssistantRealtime } from './useAssistantRealtime.js'
import { appDateParts, formatAppDateTime } from './appTimezone.js'

export function useMessageTimeline({ messages, activeConversationId, transformServerMessages, includeQueued = false }) {
  // 稳定的消息 key：按对象身份分配自增 id
  const msgUidMap = new WeakMap()
  let msgUidCounter = 0
  const getMsgKey = (msg) => {
    let uid = msgUidMap.get(msg)
    if (uid === undefined) {
      uid = `msg-${++msgUidCounter}`
      msgUidMap.set(msg, uid)
    }
    return uid
  }

  // 每条用户消息上方展示发送时间，助手回复通过过程块展示执行时长。
  const getMessageTime = (msg) => {
    if (!msg?.time) return null
    const date = new Date(msg.time)
    return Number.isNaN(date.getTime()) ? null : date
  }
  const formatMessageTime = (date) => {
    const now = appDateParts(new Date()), at = appDateParts(date)
    const hm = `${at.hour}:${at.minute}`
    const startOfDay = parts => Date.UTC(Number(parts.year), Number(parts.month) - 1, Number(parts.day))
    const dayDiff = Math.round((startOfDay(now) - startOfDay(at)) / 86400000)
    if (dayDiff <= 0) return hm
    if (dayDiff === 1) return `昨天 ${hm}`
    if (at.year === now.year) return `${at.month}月${at.day}日 ${hm}`
    return `${at.year}年${at.month}月${at.day}日 ${hm}`
  }

  // 把「用户提问 → 最终回答」之间的中间过程（工具调用、任务移交、中间 assistant 片段）
  // 聚合为可折叠的过程块，最终回答（assistant 文本 / 图表）保持原位突出展示
  // 最终回复的定位锚点：最后一条 assistant 文本消息（图表等结果消息不算）
  const isReplyTextMessage = (msg) => msg.role === 'assistant' && !msg.chartOption && Boolean((msg.content || '').trim())
  const isToolStep = (msg) => msg.kind === 'tool-run' || msg.kind === 'handoff' || msg.role === 'tool'
  const isProcessStep = (msg) => isToolStep(msg) || isReplyTextMessage(msg)
  const isResultMessage = (msg) => msg.role === 'assistant'

  // 历史接口只返回时长摘要；展开时按会话和记录范围加载详情并缓存。
  // tick 用于触发 groupedMessages 重算。
  const processDetailCache = new Map()
  const processDetailTick = ref(0)
  const loadingProcessJobs = reactive(new Set())
  const processDetailErrors = reactive(new Map())
  const realtime = useAssistantRealtime()
  const processSubscriptions = new Map()
  let detailGeneration = 0
  const stopSubscriptions = () => {
    detailGeneration += 1
    for (const subscription of processSubscriptions.values()) subscription.stop()
    processSubscriptions.clear()
  }
  if (getCurrentInstance()) {
    watch(activeConversationId, stopSubscriptions, { flush: 'sync' })
    onBeforeUnmount(stopSubscriptions)
  }

  const mergeLiveSteps = (cached, pending) => {
    const steps = [...cached]
    for (const step of pending.filter(step => step.kind !== 'process-summary')) {
      const index = steps.findIndex(existing => existing === step
        || (step.id != null && existing.id === step.id && existing.kind === step.kind)
        || (step.messageId != null && existing.messageId === step.messageId && existing.kind === step.kind))
      if (index < 0) steps.push(step)
      else if (steps[index].status === 'running' || step.status !== 'running') steps[index] = step
    }
    return steps
  }

  const groupedMessages = computed(() => {
    void processDetailTick.value
    const list = orderMessagesByTurn(messages.value, { includeQueued })
    const items = []
    const jobTiming = new Map()
    for (const message of list) {
      const jobId = message.jobId || message.generationJobId
      if (!jobId) continue
      const timing = jobTiming.get(jobId) || {}
      if (message.jobStartedAt) timing.startedAt = message.jobStartedAt
      if (message.jobCompletedAt) timing.completedAt = message.jobCompletedAt
      const status = message.jobStatus || message.processingStatus
      if (status) timing.status = status
      jobTiming.set(jobId, timing)
    }
    const jobProcessCounts = new Map()
    const getProcessKey = (jobId, firstStep) => {
      // 新回复会把前面的 assistant 片段移入过程块；恢复快照也会替换消息对象。
      // 按会话、任务和块序号标识过程，避免首条步骤变化时重建组件、丢失展开状态。
      if (jobId) {
        const index = jobProcessCounts.get(jobId) || 0
        jobProcessCounts.set(jobId, index + 1)
        return `process-${JSON.stringify([activeConversationId.value, jobId, index])}`
      }
      return `process-${JSON.stringify([activeConversationId.value, getMsgKey(firstStep)])}`
    }
    const pushItem = (item) => {
      const time = item.type === 'msg' && item.msg.role === 'user' ? getMessageTime(item.msg) : null
      items.push({
        ...item,
        timeLabel: time ? formatMessageTime(time) : '',
        timeValue: time ? time.toISOString() : '',
        timeTitle: time ? formatAppDateTime(time) : '',
      })
    }
    let i = 0
    while (i < list.length) {
      const msg = list[i]
      if (msg.role === 'user') {
        pushItem({ type: 'msg', msg, key: getMsgKey(msg) })
        i += 1
        continue
      }
      // 一轮回答：到下一个用户消息为止
      let j = i
      while (j < list.length && list[j].role !== 'user') j += 1
      const segment = list.slice(i, j)
      // 结果区以「最后一条 assistant 文本消息」定位：它之后的工具步骤（如收尾命令）
      // 仍收进过程块，保证回复是外面最后一条可见消息；整轮没有文本回复时退回旧规则
      let resultStart = segment.length
      for (let k = segment.length - 1; k >= 0; k -= 1) {
        if (isReplyTextMessage(segment[k])) { resultStart = k; break }
      }
      while (resultStart > 0 && isResultMessage(segment[resultStart - 1])) resultStart -= 1
      let pendingSteps = []
      const flushSteps = () => {
        if (!pendingSteps.length) return
        const firstStep = pendingSteps[0]
        const jobId = pendingSteps.find((step) => step.jobId)?.jobId || null
        const ranges = pendingSteps.map(step => step.processRange).filter(Boolean)
        const detailQuery = ranges.length ? {
          start_id: Math.min(...ranges.map(range => range.start_id)),
          end_id: Math.max(...ranges.map(range => range.end_id)),
        } : {}
        if (jobId) detailQuery.job_id = jobId
        // The range's end grows during generation; keep its first-click cache.
        const detailIdentity = { ...detailQuery }
        delete detailIdentity.end_id
        const detailKey = `${activeConversationId.value}:${new URLSearchParams(detailIdentity)}`
        const cached = processDetailCache.get(detailKey)
        const steps = cached ? mergeLiveSteps(cached, pendingSteps) : pendingSteps
        const timing = jobTiming.get(jobId)
        // Measure the whole reply, including model time after the final tool.
        // Persisted job boundaries override message times, which can be batched at save time.
        const timingSteps = segment.filter(step => !jobId || step.jobId === jobId)
        if (timing?.startedAt || timing?.completedAt) timingSteps.push({ kind: 'job-timing', ...timing })
        const running = ['running', 'queued', 'cancelling'].includes(timing?.status)
          || (!timing?.completedAt && pendingSteps.some(step => step.status === 'running'))
        pushItem(
          {
            type: 'process',
            key: getProcessKey(jobId, firstStep),
            steps,
            timingSteps,
            running,
            jobId,
            detailKey,
            detailQuery,
            needsDetail: !cached && steps.some((step) => step.detailOmitted),
          },
        )
        pendingSteps = []
      }
      segment.slice(0, resultStart).forEach((stepMsg) => {
        if (isProcessStep(stepMsg)) {
          if (pendingSteps.length && pendingSteps[0].jobId !== stepMsg.jobId) flushSteps()
          pendingSteps.push(stepMsg)
        } else {
          flushSteps()
          pushItem({ type: 'msg', msg: stepMsg, key: getMsgKey(stepMsg) })
        }
      })
      // 结果区里夹带的工具步骤并入过程块并排在回复之前，避免回复外面还挂着命令
      const tailResults = []
      segment.slice(resultStart).forEach((resultMsg) => {
        if (isToolStep(resultMsg)) {
          if (pendingSteps.length && pendingSteps[0].jobId !== resultMsg.jobId) flushSteps()
          pendingSteps.push(resultMsg)
        } else {
          tailResults.push(resultMsg)
        }
      })
      flushSteps()
      tailResults.forEach((resultMsg) => {
        pushItem({ type: 'msg', msg: resultMsg, key: getMsgKey(resultMsg) })
      })
      i = j
    }
    return items
  })

  // 展开过程块时按需拉取该生成任务的过程步骤概况（名称、状态、handoff 子步骤）
  const storeProcessDetail = (item, data) => {
    // A merged range can span the final reply when tool results arrive later.
    // Keep messages already rendered outside the process out of its details.
    const visibleMessageIds = new Set(groupedMessages.value
      .filter(group => group.type === 'msg' && group.msg.messageId != null)
      .map(group => group.msg.messageId))
    const records = (data.messages || []).filter(record => !visibleMessageIds.has(record.id))
    const running = records.some(record => ['running', 'queued', 'cancelling'].includes(record.processing_status))
    const steps = transformServerMessages(records, { preserveRunning: running }).filter(isProcessStep)
    const jobState = (data.messages || []).find(record => record.processing_status)
    const status = jobState?.processing_status
    if (status) {
      for (const step of item.timingSteps) {
        if (step.detailOmitted) step.status = status
      }
      for (const message of messages.value) {
        if (!item.jobId || (message.jobId !== item.jobId && message.generationJobId !== item.jobId)) continue
        message.jobStatus = status
        if (jobState.processing_started_at) message.jobStartedAt = jobState.processing_started_at
        if (jobState.processing_completed_at) message.jobCompletedAt = jobState.processing_completed_at
      }
    }
    processDetailCache.set(item.detailKey, steps)
    processDetailErrors.delete(item.detailKey)
    processDetailTick.value += 1
  }
  const subscribeProcessDetail = (item, conversationId) => {
    if (!realtime) return
    const params = { conversation_id: conversationId, ...item.detailQuery }
    const paramsKey = JSON.stringify(params)
    const previous = processSubscriptions.get(item.detailKey)
    if (previous?.paramsKey === paramsKey) return
    previous?.stop()
    let subscription
    const entry = { paramsKey, stop: () => subscription?.stop(), refresh: () => subscription?.refresh() }
    processSubscriptions.set(item.detailKey, entry)
    const isCurrent = () => activeConversationId.value === conversationId && processSubscriptions.get(item.detailKey) === entry
    subscription = realtime.subscribe('process', params, data => {
      if (isCurrent()) storeProcessDetail(item, data)
    }, error => {
      if (isCurrent()) processDetailErrors.set(item.detailKey, error)
    })
    if (!isCurrent()) subscription.stop()
  }
  if (getCurrentInstance()) {
    watch(groupedMessages, items => {
      for (const item of items) {
        if (item.type === 'process' && processDetailCache.has(item.detailKey)) subscribeProcessDetail(item, activeConversationId.value)
      }
    })
  }
  const loadProcessDetail = async (item) => {
    if (item.type !== 'process' || (!item.jobId && !item.detailQuery?.start_id)) return
    const conversationId = activeConversationId.value
    if (!conversationId || loadingProcessJobs.has(item.detailKey)) return
    if (processDetailCache.has(item.detailKey)) {
      if (processDetailErrors.has(item.detailKey)) {
        processDetailErrors.delete(item.detailKey)
        const subscription = processSubscriptions.get(item.detailKey)
        if (subscription) { subscription.refresh(); return }
      }
      subscribeProcessDetail(item, conversationId)
      return
    }
    if (!item.needsDetail) return
    const generation = detailGeneration
    loadingProcessJobs.add(item.detailKey)
    processDetailErrors.delete(item.detailKey)
    try {
      const resp = await apiFetch(
        `/api/assistant/conversations/${conversationId}/process?${new URLSearchParams(item.detailQuery)}`,
        { credentials: 'include' },
      )
      if (!resp.ok) throw new Error((await resp.text()) || '加载过程详情失败')
      const data = await resp.json()
      if (activeConversationId.value !== conversationId || generation !== detailGeneration) return
      storeProcessDetail(item, data)
      subscribeProcessDetail(item, conversationId)
    } catch (err) {
      processDetailErrors.set(item.detailKey, err.message || '加载过程详情失败')
    } finally {
      loadingProcessJobs.delete(item.detailKey)
    }
  }

  return { groupedMessages, loadingProcessJobs, processDetailErrors, loadProcessDetail }
}
