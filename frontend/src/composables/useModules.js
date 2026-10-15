import { computed, inject, onBeforeUnmount, provide, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'

const modulesKey = Symbol('modules')

export async function moduleRequest(path = '', options = {}) {
  const response = await apiFetch('/api/modules' + path, options)
  const data = await response.json()
  if (!response.ok) throw new Error(data.error || '模块请求失败')
  return data
}
export const jsonRequest = (method, value) => ({ method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value) })

export function createModulesState(request = moduleRequest, now = () => Date.now()) {
  const modules = ref([]), diagnostics = ref([]), loading = ref(false), error = ref('')
  const indicators = ref({}), clock = ref(now())
  const due = new Map(), controllers = new Set()
  let revision = 0, loadPending = null, indicatorPending = null
  const activeIndicators = computed(() => modules.value.filter(module => module.enabled).flatMap(module => {
    const indicator = indicators.value[module.id]
    return indicator && indicator.starts <= clock.value && indicator.expires > clock.value ? [{ ...indicator, module_id: module.id }] : []
  }))
  const indicatorDescription = computed(() => activeIndicators.value.map(value => value.label).join(' · '))
  const tick = () => { clock.value = now() }
  const call = async (path, options = {}, timeout = 0) => {
    const controller = new AbortController()
    controllers.add(controller)
    const timer = timeout ? setTimeout(() => controller.abort(), timeout) : null
    try { return await request(path, { ...options, signal: controller.signal }) }
    finally { clearTimeout(timer); controllers.delete(controller) }
  }
  const refreshIndicators = () => {
    tick()
    if (indicatorPending) return indicatorPending
    const current = revision
    const queue = modules.value.filter(module => module.enabled && module.indicator && (due.get(module.id) || 0) <= now())
    const worker = async () => {
      while (queue.length && current === revision) {
        const module = queue.shift()
        due.set(module.id, now() + Math.max(30, module.indicator.refresh_secs || 60) * 1000)
        try {
          const data = await call(`/${encodeURIComponent(module.id)}/indicator`, {}, 20000)
          if (current !== revision) continue
          const receivedAt = now()
          const value = data.indicator
          const starts = value?.starts_at ? Date.parse(value.starts_at) : receivedAt
          const expires = Date.parse(value?.expires_at)
          const valid = typeof value?.label === 'string' && value.label.length > 0 && value.label.length <= 200
            && Number.isFinite(starts) && Number.isFinite(expires) && expires > starts && expires <= receivedAt + 86400000
          // Use the response time so an immediate indication is already active when applied.
          clock.value = receivedAt
          indicators.value = { ...indicators.value, [module.id]: valid ? {
            label: value.label, starts, expires,
            icon: typeof value.icon === 'string' && /^[a-z-]{1,40}$/.test(value.icon) ? value.icon : 'activity',
            tone: ['info', 'warning', 'error', 'success'].includes(value.tone) ? value.tone : 'info',
          } : null }
        } catch { /* Preserve only a previously received, unexpired indication. */ }
      }
    }
    const pending = Promise.all(Array.from({ length: Math.min(4, queue.length) }, worker))
      .finally(() => { if (indicatorPending === pending) indicatorPending = null })
    indicatorPending = pending
    return pending
  }
  const invalidateIndicators = () => {
    due.clear()
    // A write may finish while an older status query is in flight.
    return indicatorPending ? indicatorPending.then(refreshIndicators) : refreshIndicators()
  }
  const signature = module => JSON.stringify([module.enabled, module.version, module.settings, module.indicator])
  const apply = (data, changedId) => {
    const previous = new Map(modules.value.map(module => [module.id, module]))
    const next = data.modules || []
    indicators.value = Object.fromEntries(next.filter(module => module.enabled && module.id !== changedId
      && previous.has(module.id) && signature(previous.get(module.id)) === signature(module)).map(module => [module.id, indicators.value[module.id]]))
    modules.value = next; diagnostics.value = data.diagnostics || []
    due.clear(); indicatorPending = null
    void refreshIndicators()
  }
  const reset = () => {
    revision++; for (const controller of controllers) controller.abort()
    controllers.clear(); due.clear(); loadPending = indicatorPending = null
    modules.value = []; diagnostics.value = []; indicators.value = {}; loading.value = false; error.value = ''
  }
  const load = (reload = false) => {
    if (loadPending && !reload) return loadPending
    const current = ++revision
    loading.value = true
    error.value = ''
    const pending = (async () => {
      try {
        const data = await call(reload ? '/reload' : '', reload ? jsonRequest('POST', {}) : {})
        if (current === revision) apply(data)
      } catch (cause) { if (current === revision) error.value = cause.message }
      finally { if (current === revision) loading.value = false; if (loadPending === pending) loadPending = null }
    })()
    loadPending = pending
    return pending
  }
  const save = async (id, value) => {
    const current = ++revision
    loadPending = null; loading.value = false; error.value = ''
    const data = await call('/' + encodeURIComponent(id), jsonRequest('PUT', value))
    if (current !== revision) return
    apply(data, id)
  }
  return { modules, diagnostics, loading, error, load, save, reset, tick, refreshIndicators, invalidateIndicators, activeIndicators, indicatorDescription }
}

export function provideModules(ready) {
  const state = createModulesState()
  watch(ready, enabled => { if (enabled) state.load(); else state.reset() }, { immediate: true })
  const timer = setInterval(() => {
    state.tick()
    if (ready.value && document.visibilityState !== 'hidden') state.refreshIndicators()
  }, 15000)
  onBeforeUnmount(() => { clearInterval(timer); state.reset() })
  provide(modulesKey, state)
  return state
}
export const useModules = () => inject(modulesKey, null) || createModulesState()
