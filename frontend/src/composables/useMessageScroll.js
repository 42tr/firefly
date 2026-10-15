import { nextTick, onBeforeUnmount, onBeforeUpdate, onUpdated, ref, watch } from 'vue'

export function useMessageScroll() {
  const chatPanel = ref(null)
  const isPinnedToBottom = ref(true)
  let frame = null
  let previousFrameTime = 0
  let writtenTop = null
  let observedTop = 0
  let touchY = null
  let observer = null
  let removeListeners = () => {}
  const reservedReplies = new Set()
  let reserveRevision = 0
  let layoutBeforeUpdate = null

  const cancelFollow = () => {
    if (frame !== null) cancelAnimationFrame(frame)
    frame = null
    previousFrameTime = 0
  }
  const setPinned = pinned => {
    isPinnedToBottom.value = pinned
    // While following, this controller owns positioning. Restore native scroll
    // anchoring when the user is reading history (including image loads above).
    if (chatPanel.value) chatPanel.value.style.overflowAnchor = pinned ? 'none' : ''
    if (!pinned) cancelFollow()
  }
  const writeTop = (panel, top) => {
    panel.scrollTop = top
    writtenTop = panel.scrollTop
    observedTop = writtenTop
  }
  const follow = time => {
    frame = null
    const panel = chatPanel.value
    if (!panel || !isPinnedToBottom.value) return
    const target = Math.max(0, panel.scrollHeight - panel.clientHeight)
    const distance = target - panel.scrollTop
    const elapsed = previousFrameTime ? Math.min(64, time - previousFrameTime) : 16
    previousFrameTime = time
    const reducedMotion = globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    // One short catch-up animation follows the latest target; new deltas never
    // start overlapping native smooth-scroll animations. Bound the trailing gap.
    const top = reducedMotion || distance <= 1 ? target
      : target - Math.min(distance, 120) * Math.exp(-elapsed / 45)
    writeTop(panel, top)
    if (Math.abs(target - panel.scrollTop) > 1) scheduleFollow()
    else previousFrameTime = 0
  }
  const scheduleFollow = () => {
    if (chatPanel.value && isPinnedToBottom.value && frame === null) frame = requestAnimationFrame(follow)
  }
  const releaseReplySpace = () => {
    const panel = chatPanel.value
    if (!panel || !reservedReplies.size) return
    const viewport = panel.getBoundingClientRect()
    const anchor = [...panel.querySelectorAll('.message-content, .process-group-header')].find(element => {
      const rect = element.getBoundingClientRect()
      return rect.bottom > viewport.top && rect.top < viewport.bottom
    })
    const before = anchor?.getBoundingClientRect().top
    const top = panel.scrollTop
    const anchoring = panel.style.overflowAnchor
    panel.style.overflowAnchor = 'none'
    for (const element of reservedReplies) element.style.minHeight = ''
    reservedReplies.clear()
    reserveRevision += 1
    if (anchor) writeTop(panel, top + anchor.getBoundingClientRect().top - before)
    panel.style.overflowAnchor = anchoring
  }
  // Replacing a long reply must not shorten the scroll range under the reader.
  // The stable outer element keeps its footprint and bottom-aligns new content.
  // Reclaim the temporary space on deliberate scrolling or a new turn.
  const vStableReply = {
    mounted(element, { value }) {
      if (value) releaseReplySpace()
    },
    beforeUpdate(element, { value, oldValue }) {
      if (!value || value === oldValue || !isPinnedToBottom.value) return
      element.style.minHeight = `${element.offsetHeight}px`
      reservedReplies.add(element)
    },
    beforeUnmount(element) { reservedReplies.delete(element) },
  }
  // Finishing a turn can also remove its workflow above the reply. Preserve
  // the whole scroll range before paint, unless space was deliberately freed.
  onBeforeUpdate(() => {
    const panel = chatPanel.value
    layoutBeforeUpdate = panel && isPinnedToBottom.value && reservedReplies.size
      ? { panel, height: panel.scrollHeight, top: panel.scrollTop, revision: reserveRevision } : null
  })
  onUpdated(() => {
    const before = layoutBeforeUpdate
    layoutBeforeUpdate = null
    if (!before || before.panel !== chatPanel.value || !isPinnedToBottom.value
      || before.revision !== reserveRevision || !reservedReplies.size) return
    const shrink = before.height - before.panel.scrollHeight
    if (shrink <= 0) return
    const element = [...reservedReplies].at(-1)
    element.style.minHeight = `${element.offsetHeight + shrink}px`
    writeTop(before.panel, before.top)
  })
  const releaseFollow = () => {
    setPinned(false)
    releaseReplySpace()
  }
  const handleScroll = () => {
    const panel = chatPanel.value
    if (!panel) return
    const top = panel.scrollTop
    if ((writtenTop !== null && Math.abs(top - writtenTop) <= 1)
      || (isPinnedToBottom.value && frame !== null)) {
      observedTop = top
      return
    }
    if (top < observedTop - 1) releaseFollow()
    else if (top > observedTop && panel.scrollHeight - top - panel.clientHeight < 120) {
      setPinned(true)
      scheduleFollow()
    }
    observedTop = panel.scrollTop
  }
  const handleWheel = event => { if (event.deltaY < 0) releaseFollow() }
  const handleTouchStart = event => { touchY = event.touches[0]?.clientY ?? null }
  const handleTouchMove = event => {
    const y = event.touches[0]?.clientY
    if (touchY !== null && y > touchY) releaseFollow()
    touchY = y ?? null
  }
  const handleKeydown = event => {
    if (event.target?.closest?.('input, textarea, select, [contenteditable="true"]')) return
    if (['ArrowUp', 'PageUp', 'Home'].includes(event.key) || (event.key === ' ' && event.shiftKey)) releaseFollow()
  }
  const handlePointerDown = event => {
    const panel = chatPanel.value
    if (event.target === panel && event.clientX >= panel.getBoundingClientRect().left + panel.clientLeft + panel.clientWidth) releaseFollow()
  }

  watch(chatPanel, panel => {
    removeListeners()
    observer?.disconnect()
    observer = null
    cancelFollow()
    writtenTop = null
    for (const element of reservedReplies) element.style.minHeight = ''
    reservedReplies.clear()
    if (!panel) return
    observedTop = panel.scrollTop
    panel.style.overflowAnchor = isPinnedToBottom.value ? 'none' : ''
    const listeners = { scroll: handleScroll, wheel: handleWheel, touchstart: handleTouchStart,
      touchmove: handleTouchMove, keydown: handleKeydown, pointerdown: handlePointerDown }
    for (const [name, handler] of Object.entries(listeners)) panel.addEventListener(name, handler, { passive: true })
    removeListeners = () => {
      for (const [name, handler] of Object.entries(listeners)) panel.removeEventListener(name, handler)
    }
    if (typeof ResizeObserver !== 'undefined') {
      observer = new ResizeObserver(scheduleFollow)
      observer.observe(panel)
      if (panel.firstElementChild) observer.observe(panel.firstElementChild)
    }
    scheduleFollow()
  }, { flush: 'post' })

  const scrollToBottom = async ({ force = false } = {}) => {
    await nextTick()
    if (force) {
      cancelFollow()
      releaseReplySpace()
      setPinned(true)
      const panel = chatPanel.value
      if (panel) writeTop(panel, Math.max(0, panel.scrollHeight - panel.clientHeight))
    } else scheduleFollow()
  }
  onBeforeUnmount(() => {
    cancelFollow()
    observer?.disconnect()
    removeListeners()
    reservedReplies.clear()
  })
  return { chatPanel, isPinnedToBottom, scrollToBottom, vStableReply }
}
