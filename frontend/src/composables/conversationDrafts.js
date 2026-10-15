// File objects stay in memory; only text survives a page reload in this tab.
export function createConversationDrafts(storage, key = 'assistant:conversation-drafts') {
  const attachments = new Map()
  let texts = {}
  try {
    const saved = JSON.parse(storage?.getItem(key) || '{}')
    if (saved && typeof saved === 'object' && !Array.isArray(saved)) texts = saved
  } catch { /* Storage may be unavailable. */ }
  const persist = () => {
    try { storage?.setItem(key, JSON.stringify(texts)) } catch { /* Keep in-memory drafts. */ }
  }
  return {
    has(id) {
      return Boolean((typeof texts[id] === 'string' && texts[id].trim()) || attachments.get(String(id))?.length)
    },
    save(id, text, files) {
      if (id == null) return
      if (text) texts[id] = text
      else delete texts[id]
      attachments.set(String(id), [...files])
      persist()
    },
    restore(id) {
      const files = attachments.get(String(id)) || []
      attachments.delete(String(id))
      return { text: typeof texts[id] === 'string' ? texts[id] : '', attachments: files }
    },
    delete(id) {
      delete texts[id]
      for (const file of attachments.get(String(id)) || []) {
        if (file.previewUrl) URL.revokeObjectURL(file.previewUrl)
      }
      attachments.delete(String(id))
      persist()
    },
    dispose() {
      for (const files of attachments.values()) {
        for (const file of files) if (file.previewUrl) URL.revokeObjectURL(file.previewUrl)
      }
      attachments.clear()
    },
  }
}
