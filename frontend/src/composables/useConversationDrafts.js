import { ref, watch, onMounted, onBeforeUnmount } from 'vue'
import { createConversationDrafts } from './conversationDrafts.js'

export function useConversationDrafts({ activeConversationId, input, chatAttachments }) {
  let storage
  try { storage = window.sessionStorage } catch { /* In-memory drafts still work. */ }
  const inputDrafts = createConversationDrafts(storage)
  const draftRevision = ref(0)
  const saveCurrentDraft = () => {
    inputDrafts.save(activeConversationId.value, input.value, chatAttachments.value)
    draftRevision.value += 1
  }
  const restoreDraft = (conversationId) => {
    if (activeConversationId.value === conversationId) return
    saveCurrentDraft()
    const draft = inputDrafts.restore(conversationId)
    activeConversationId.value = conversationId
    input.value = draft.text
    chatAttachments.value = draft.attachments
  }
  watch([input, chatAttachments], saveCurrentDraft, { deep: true, flush: 'post' })
  onMounted(() => window.addEventListener('pagehide', saveCurrentDraft))
  onBeforeUnmount(() => {
    saveCurrentDraft()
    inputDrafts.dispose()
    window.removeEventListener('pagehide', saveCurrentDraft)
  })
  return { inputDrafts, draftRevision, restoreDraft }
}
