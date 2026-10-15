import { nextTick, onBeforeUnmount, ref, watch } from 'vue'

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
  const releaseFollow = () => setPinned(false)
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
    observedTop = top
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
      setPinned(true)
      const panel = chatPanel.value
      if (panel) writeTop(panel, Math.max(0, panel.scrollHeight - panel.clientHeight))
    } else scheduleFollow()
  }
  onBeforeUnmount(() => {
    cancelFollow()
    observer?.disconnect()
    removeListeners()
  })
  return { chatPanel, isPinnedToBottom, scrollToBottom }
}
