import { onScopeDispose, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'
import { activeTask } from './taskTree.js'

// The endpoint returns a whole conversation, so every reply shares its cursor
// and request. Entries live only while at least one visible tree uses them.
export function createConversationTasks(fetcher = apiFetch, timers = globalThis) {
  const conversations = new Map()
  const publish = entry => {
    for (const subscriber of entry.subscribers) subscriber.receive(entry.tasks, entry.error)
  }
  const polling = entry => [...entry.subscribers].some(subscriber =>
    ['running', 'queued'].includes(subscriber.replyStatus)
    || entry.tasks.some(task => task.generation_job_id === subscriber.jobId && activeTask(task)))
  function schedule(entry, delay, initial = false) {
    timers.clearTimeout(entry.timer)
    entry.timer = timers.setTimeout(() => { entry.timer = null; load(entry, initial) }, delay)
  }
  async function load(entry, initial) {
    if (!entry.subscribers.size || entry.controller) return
    const version = entry.epoch
    const controller = new AbortController()
    entry.controller = controller
    try {
      const response = await fetcher(`/api/assistant/conversations/${entry.id}/tasks?after=${entry.cursor}&tools_after=${entry.toolCursor}&wait_secs=${initial ? 0 : 20}`,
        { signal: controller.signal, credentials: 'include' })
      if (!response.ok) throw new Error('读取任务状态失败')
      const page = await response.json()
      if (version !== entry.epoch) return
      entry.tasks = page.tasks || []
      entry.cursor = page.cursor || entry.cursor
      entry.toolCursor = page.tool_cursor ?? entry.toolCursor
      entry.error = ''
      publish(entry)
      if (page.has_more || polling(entry)) schedule(entry, page.has_more ? 0 : 250)
    } catch (cause) {
      if (cause.name === 'AbortError' || version !== entry.epoch) return
      entry.error = cause.message
      publish(entry)
      schedule(entry, 3000)
    } finally {
      if (entry.controller === controller) entry.controller = null
    }
  }
  function refresh(entry) {
    entry.epoch++
    entry.controller?.abort()
    entry.controller = null
    schedule(entry, 0, true)
  }
  function subscribe(id, jobId, replyStatus, receive) {
    let entry = conversations.get(id)
    if (!entry) {
      entry = { id, cursor: 0, toolCursor: 0, epoch: 0, tasks: [], error: '', subscribers: new Set(), controller: null, timer: null }
      conversations.set(id, entry)
    }
    const subscriber = { jobId, replyStatus, receive }
    entry.subscribers.add(subscriber)
    receive(entry.tasks, entry.error)
    if (!entry.controller && entry.timer === null) schedule(entry, 0, true)
    return {
      update(status) {
        if (subscriber.replyStatus === status) return
        subscriber.replyStatus = status
        refresh(entry)
      },
      refresh: () => refresh(entry),
      stop() {
        entry.subscribers.delete(subscriber)
        if (entry.subscribers.size) return
        entry.epoch++
        entry.controller?.abort()
        timers.clearTimeout(entry.timer)
        conversations.delete(id)
      },
    }
  }
  return { subscribe }
}

const sharedTasks = createConversationTasks()

export function useConversationTasks(conversationId, jobId, replyStatus, active, source = sharedTasks) {
  const tasks = ref([]), error = ref('')
  let subscription, identity
  const stop = watch(() => [conversationId(), jobId(), active(), replyStatus()], ([id, job, enabled, status]) => {
    const next = enabled && id && job ? JSON.stringify([id, job]) : null
    if (next !== identity) {
      subscription?.stop()
      subscription = null
      identity = next
      tasks.value = []
      error.value = ''
      if (next) subscription = source.subscribe(id, job, status, (snapshot, message) => {
        tasks.value = snapshot
        error.value = message
      })
    } else subscription?.update(status)
  }, { immediate: true })
  onScopeDispose(() => { stop(); subscription?.stop() })
  return { tasks, error, refresh: () => subscription?.refresh() }
}
