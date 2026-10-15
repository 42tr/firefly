import { onBeforeUnmount, onMounted, ref } from 'vue'

// iOS can pan the visual viewport while leaving the layout viewport unchanged.
// Track both its size and its position, independently of input focus.
export function useMobileViewport() {
  const viewportStyle = ref(null)
  let viewport
  let frame = null

  const sync = () => {
    const current = window.visualViewport
    if (window.innerWidth > 768 || !current || !(current.height > 0)
      || Math.abs((current.scale || 1) - 1) > 0.01) {
      viewportStyle.value = null
      return
    }
    viewportStyle.value = {
      position: 'fixed',
      top: `${Math.max(0, current.offsetTop || 0)}px`,
      left: '0px',
      right: '0px',
      height: `${current.height}px`,
      '--app-viewport-height': `${current.height}px`,
      '--composer-bottom-inset': current.height < window.innerHeight - 100
        ? '0px' : 'env(safe-area-inset-bottom)',
    }
  }
  const schedule = () => {
    if (frame !== null) return
    frame = window.requestAnimationFrame(() => {
      frame = null
      sync()
    })
  }

  onMounted(() => {
    viewport = window.visualViewport
    sync()
    viewport?.addEventListener('resize', schedule)
    viewport?.addEventListener('scroll', schedule)
    window.addEventListener('resize', schedule)
  })
  onBeforeUnmount(() => {
    viewport?.removeEventListener('resize', schedule)
    viewport?.removeEventListener('scroll', schedule)
    window.removeEventListener('resize', schedule)
    if (frame !== null) window.cancelAnimationFrame(frame)
  })

  return { viewportStyle }
}
