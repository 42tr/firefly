import { ref, watch, onMounted, onBeforeUnmount } from 'vue'
import { createConversationDrafts } from './conversationDrafts.js'

// Typing must not write storage and rebuild the conversation list per
// keystroke: persist on a short delay, and always flush synchronously when
// the active conversation changes or the page goes away.
const DRAFT_SAVE_DELAY_MS = 400

export function useConversationDrafts({ activeConversationId, input, chatAttachments }) {
  let storage
  try { storage = window.sessionStorage } catch { /* In-memory drafts still work. */ }
  const inputDrafts = createConversationDrafts(storage)
  const draftRevision = ref(0)
  let saveTimer = null
  // The conversation the pending debounced save belongs to; a flush after the
  // active conversation changed would otherwise store text under the wrong id.
  let pendingDraftId

  const saveDraftFor = (id) => {
    const hadDraft = inputDrafts.has(id)
    inputDrafts.save(id, input.value, chatAttachments.value)
    // Sidebar filtering only depends on whether a draft exists, not its text.
    if (inputDrafts.has(id) !== hadDraft) draftRevision.value += 1
  }
  const cancelPendingSave = () => {
    if (saveTimer != null) {
      clearTimeout(saveTimer)
      saveTimer = null
    }
    pendingDraftId = undefined
  }
  const saveCurrentDraft = () => {
    cancelPendingSave()
    saveDraftFor(activeConversationId.value)
  }
  const scheduleDraftSave = () => {
    pendingDraftId = activeConversationId.value
    if (saveTimer != null) return
    saveTimer = setTimeout(() => {
      saveTimer = null
      const id = pendingDraftId
      pendingDraftId = undefined
      if (id == null || id !== activeConversationId.value) return
      saveDraftFor(id)
    }, DRAFT_SAVE_DELAY_MS)
  }
  const restoreDraft = (conversationId) => {
    if (activeConversationId.value === conversationId) return
    saveCurrentDraft()
    const draft = inputDrafts.restore(conversationId)
    activeConversationId.value = conversationId
    input.value = draft.text
    chatAttachments.value = draft.attachments
    draftRevision.value += 1
  }
  watch([input, chatAttachments], scheduleDraftSave, { deep: true, flush: 'post' })
  onMounted(() => window.addEventListener('pagehide', saveCurrentDraft))
  onBeforeUnmount(() => {
    saveCurrentDraft()
    inputDrafts.dispose()
    window.removeEventListener('pagehide', saveCurrentDraft)
  })
  return { inputDrafts, draftRevision, restoreDraft }
}
