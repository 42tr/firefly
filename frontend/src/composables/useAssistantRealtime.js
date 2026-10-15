import { getCurrentInstance, hasInjectionContext, inject, onBeforeUnmount, provide, ref, toValue, watch } from 'vue'
import { apiFetch } from '@/utils/http'

const realtimeKey = Symbol('assistant-realtime')

// One connection per app, shared subscriptions, fresh snapshots after every reconnect.
export function createAssistantRealtime({ WebSocketClass = globalThis.WebSocket, fetcher = apiFetch,
  href = () => window.location.href, now = Date.now } = {}) {
  const connected = ref(false)
  const subscriptions = new Map()
  let socket, retryTimer, watchdog, running = false, generation = 0, attempts = 0, nextId = 0, lastMessage = 0
  const send = value => { if (socket?.readyState === 1) socket.send(JSON.stringify(value)) }
  const sendSubscription = entry => send({ type: 'subscribe', id: entry.id, topic: entry.topic, params: entry.params })
  const schedule = current => {
    if (!running || current !== generation) return
    clearTimeout(retryTimer)
    retryTimer = setTimeout(() => connect(current), Math.min(30000, 1000 * 2 ** Math.min(attempts++, 5)))
  }
  const connect = async current => {
    if (!running || current !== generation) return
    if (attempts > 0) {
      try { const result = await fetcher('/api/assistant/ping'); if (!result.ok) { schedule(current); return } }
      catch { schedule(current); return }
      if (!running || current !== generation) return
    }
    const url = new URL('/api/assistant/ws', href())
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
    let ws
    try { ws = new WebSocketClass(url) } catch { schedule(current); return }
    socket = ws
    lastMessage = now()
    ws.onopen = () => {
      if (socket !== ws || current !== generation) return
      connected.value = true
      for (const entry of subscriptions.values()) sendSubscription(entry)
    }
    ws.onmessage = event => {
      if (socket !== ws || current !== generation) return
      let data
      try { data = JSON.parse(event.data) } catch { ws.close(); return }
      lastMessage = now()
      attempts = 0
      if (data.type === 'heartbeat') return
      const entry = [...subscriptions.values()].find(entry => entry.id === data.id)
      if (!entry) return
      if (data.type === 'snapshot' || data.type === 'snapshot_delta') {
        if (data.type === 'snapshot_delta') {
          if (!entry.hasData) { send({ type: 'refresh', id: entry.id }); return }
          const records = new Map((entry.data.messages || []).map(record => [record.id, record]))
          for (const id of data.data.removed_ids || []) records.delete(id)
          for (const record of data.data.messages || []) records.set(record.id, record)
          entry.data = { ...entry.data, conversation_id: data.data.conversation_id,
            job_id: data.data.job_id, messages: [...records.values()].sort((a, b) => a.id - b.id) }
        } else entry.data = data.data
        entry.hasData = true
        for (const listener of entry.listeners) listener.receive(entry.data)
      } else if (data.type === 'subscription_error') {
        for (const listener of entry.listeners) listener.error?.(data.message)
      }
    }
    ws.onerror = () => ws.close()
    ws.onclose = () => {
      if (socket !== ws || current !== generation) return
      socket = null
      connected.value = false
      schedule(current)
    }
  }
  const start = () => {
    if (running) return
    running = true
    attempts = 0
    connect(++generation)
    // A local liveness clock, not an API poll.
    watchdog = setInterval(() => { if (socket && now() - lastMessage > 65000) socket.close() }, 10000)
  }
  const stop = () => {
    running = false
    generation++
    clearTimeout(retryTimer)
    clearInterval(watchdog)
    const previous = socket
    socket = null
    previous?.close()
    connected.value = false
    for (const entry of subscriptions.values()) { entry.hasData = false; entry.data = undefined }
  }
  const subscribe = (topic, params, receive, error) => {
    const key = JSON.stringify([topic, params])
    let entry = subscriptions.get(key)
    if (!entry) {
      entry = { id: String(++nextId), topic, params, listeners: new Set(), hasData: false }
      subscriptions.set(key, entry)
      sendSubscription(entry)
    }
    const listener = { receive, error }
    entry.listeners.add(listener)
    if (entry.hasData) receive(entry.data)
    return {
      refresh: () => send({ type: 'refresh', id: entry.id }),
      stop: () => {
        entry.listeners.delete(listener)
        if (entry.listeners.size) return
        send({ type: 'unsubscribe', id: entry.id })
        subscriptions.delete(key)
      },
    }
  }
  return { connected, start, stop, subscribe }
}

export function provideAssistantRealtime(ready) {
  const realtime = createAssistantRealtime()
  watch(ready, value => value ? realtime.start() : realtime.stop(), { immediate: true })
  onBeforeUnmount(realtime.stop)
  provide(realtimeKey, realtime)
  return realtime
}
export const useAssistantRealtime = () => getCurrentInstance()?.provides[realtimeKey]
  || (hasInjectionContext() ? inject(realtimeKey, null) : null)

export function useRealtimeSubscription(topic, params, receive, { enabled = true, onError } = {}) {
  const realtime = useAssistantRealtime()
  let subscription
  const stop = watch(() => JSON.stringify([!!toValue(enabled), toValue(params) || {}]), value => {
    subscription?.stop()
    subscription = null
    const [active, query] = JSON.parse(value)
    if (active && realtime) subscription = realtime.subscribe(topic, query, receive, onError)
  }, { immediate: true })
  onBeforeUnmount(() => { stop(); subscription?.stop() })
  return { refresh: () => subscription?.refresh(), available: !!realtime }
}
