import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'

// Only replace the reference before the caret, preserving surrounding message text.
export function workspaceReference(text, start, end = start) {
  if (start !== end) return null
  const before = text.slice(0, start)
  const offset = before.lastIndexOf('///')
  if (offset < 0 || /[/:]/.test(before[offset - 1] || '')) return null
  const query = before.slice(offset + 3)
  if (/[\n\r]/.test(query)) return null
  const slash = query.lastIndexOf('/')
  const path = slash < 0 ? '' : query.slice(0, slash)
  if (path.split('/').some(part => part === '..' || part === '.')) return null
  return { start: offset, end: start, path, filter: query.slice(slash + 1) }
}

export function useWorkspaceCompletion({ model, inputEl, disabled, fetchWorkspace }) {
  const reference = ref(null)
  const listing = ref(null)
  const loading = ref(false)
  const error = ref('')
  const selected = ref(0)
  let request = 0
  let controller
  const open = computed(() => !!reference.value && !disabled.value)
  const entries = computed(() => (listing.value?.entries || []).filter(entry =>
    entry.name.toLocaleLowerCase().startsWith(reference.value?.filter.toLocaleLowerCase() || ''),
  ))
  const close = () => { reference.value = null }
  const sync = () => {
    const input = inputEl.value
    reference.value = input && !disabled.value
      ? workspaceReference(input.value, input.selectionStart, input.selectionEnd)
      : null
    if (reference.value) reference.value.text = input.value
  }

  watch([() => reference.value?.path, disabled], async ([path, isDisabled]) => {
    const generation = ++request
    controller?.abort()
    listing.value = null
    error.value = ''
    loading.value = false
    if (path === undefined || isDisabled) return
    controller = new AbortController()
    loading.value = true
    try {
      const data = await fetchWorkspace(path, controller.signal)
      if (!data.root?.startsWith('/')) throw new Error('无法读取 workspace 绝对路径')
      if (generation === request) listing.value = data
    } catch (err) {
      if (generation === request) error.value = err.message || '读取 workspace 失败'
    } finally {
      if (generation === request) loading.value = false
    }
  })
  watch(entries, () => { selected.value = 0 })
  watch(disabled, close)
  watch(model, () => {
    // Clearing, replacing a draft, or switching conversations invalidates the popup.
    if (reference.value && model.value !== reference.value.text) close()
  }, { flush: 'post' })

  const choose = async (entry, browse = false) => {
    const current = reference.value
    if (!current || !listing.value || !entry) return
    const replacement = browse
      ? `///${entry.path}/`
      : `${listing.value.root.replace(/\/+$/, '')}/${entry.path}`
    const text = inputEl.value.value
    const updatedText = text.slice(0, current.start) + replacement + text.slice(current.end)
    model.value = updatedText
    const caret = current.start + replacement.length
    if (browse) {
      reference.value = { ...workspaceReference(updatedText, caret), text: updatedText }
    } else {
      close()
    }
    await nextTick()
    inputEl.value?.focus()
    inputEl.value?.setSelectionRange(caret, caret)
  }
  const keydown = (event) => {
    if (!open.value) return false
    if (event.key === 'Escape') {
      event.preventDefault()
      close()
      return true
    }
    if (['ArrowDown', 'ArrowUp'].includes(event.key)) {
      event.preventDefault()
      const count = entries.value.length
      if (count) selected.value = (selected.value + (event.key === 'ArrowDown' ? 1 : count - 1)) % count
      return true
    }
    if (event.key === 'Tab' && !event.shiftKey) {
      event.preventDefault()
      choose(entries.value[selected.value], true)
      return true
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      // A leaf directory remains selectable after Tab, even without children.
      const currentDirectory = listing.value && !reference.value.filter
        ? { path: listing.value.path }
        : null
      choose(entries.value[selected.value] || currentDirectory)
      return true
    }
    return false
  }
  onBeforeUnmount(() => { request += 1; controller?.abort() })
  return { open, entries, loading, error, selected, sync, close, choose, keydown }
}
