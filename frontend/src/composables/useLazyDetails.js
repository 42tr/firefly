import { onScopeDispose, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'

const pending = status => ['running', 'queued', 'cancelling'].includes(status)

// Each mounted row owns its cache. No request is made until the user opens it.
export function useLazyDetails(url, status, active, fetcher = apiFetch) {
  const detail = ref(null), loading = ref(false), error = ref('')
  let controller, timer, epoch = 0, loadedStatus = null
  function stop() {
    epoch += 1
    controller?.abort()
    clearTimeout(timer)
    loading.value = false
  }
  async function load(force = false) {
    const endpoint = url()
    if (!active.value || !endpoint || loading.value) return
    if (!force && detail.value && loadedStatus === status() && !pending(status())) return
    const version = epoch
    controller = new AbortController()
    loading.value = true
    error.value = ''
    try {
      const response = await fetcher(endpoint, { credentials: 'include', signal: controller.signal })
      if (!response.ok) throw new Error('读取执行详情失败，请重试')
      const data = await response.json()
      if (version !== epoch || !active.value) return
      detail.value = data
      loadedStatus = status()
    } catch (cause) {
      if (version !== epoch || cause.name === 'AbortError') return
      error.value = cause.message || '读取执行详情失败，请重试'
    } finally {
      if (version === epoch) {
        loading.value = false
        // Poll live details only while open. A failed load waits for explicit retry.
        if (active.value && !error.value && pending(status())) timer = setTimeout(() => load(true), 2000)
      }
    }
  }
  watch(url, () => { stop(); detail.value = null; loadedStatus = null; error.value = ''; load() })
  watch(() => [active.value, status()], () => { stop(); if (active.value) load() }, { immediate: true })
  onScopeDispose(stop)
  return { detail, loading, error, retry: () => { stop(); load(true) } }
}
