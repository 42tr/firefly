import { onBeforeUnmount, onMounted, ref, watch } from 'vue'

export function useConversationReadState({ conversations, activeConversationId, isReading, readMessageId, saveRead }) {
  // Server snapshots are authoritative across devices. Keep only acknowledgements
  // from this session to cover snapshots that were already in flight when read.
  const acknowledged = ref({})
  const pending = ref({})
  let retryTimer = null
  let disposed = false
  const readPosition = conversation => Math.max(conversation.read_message_id || 0,
    acknowledged.value[conversation.id] || 0, pending.value[conversation.id] || 0)
  const hasUnread = conversation => conversation.status === 'completed'
    && (conversation.last_message_id || 0) > readPosition(conversation)
  const markRead = async () => {
    if (disposed || globalThis.document?.hidden || !isReading.value) return
    const conversation = conversations.value.find(item => item.id === activeConversationId.value)
    if (!conversation || conversation.status !== 'completed' || pending.value[conversation.id]) return
    // A list update can arrive before the reply is visible. Acknowledge only
    // loaded history or a finished stream, never the list's newest position.
    const messageId = Math.min(readMessageId.value, conversation.last_message_id || 0)
    if (!messageId || messageId <= readPosition(conversation)) return
    const id = conversation.id
    pending.value[id] = messageId
    try {
      const saved = await saveRead(id, messageId)
      if (!disposed) acknowledged.value[id] = Math.max(acknowledged.value[id] || 0, messageId, saved || 0)
    } catch {
      // Roll back the optimistic badge and retry while the reply is visible.
      if (!disposed && !retryTimer) retryTimer = setTimeout(() => { retryTimer = null; markRead() }, 5000)
    } finally {
      delete pending.value[id]
    }
    // A newer reply may have become visible while this request was in flight.
    if (!disposed && !retryTimer) markRead()
  }
  watch([conversations, activeConversationId, isReading, readMessageId], markRead, { deep: true, immediate: true })
  onMounted(() => globalThis.document?.addEventListener('visibilitychange', markRead))
  onBeforeUnmount(() => {
    disposed = true
    clearTimeout(retryTimer)
    globalThis.document?.removeEventListener('visibilitychange', markRead)
  })
  const forget = id => { delete acknowledged.value[id]; delete pending.value[id] }
  return { hasUnread, forget }
}
