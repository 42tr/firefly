import { computed, getCurrentScope, onScopeDispose, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'

async function request(path = '', options = {}) {
  const response = await apiFetch('/api/assistant/agent-settings' + path, options)
  const data = await response.json().catch(() => ({}))
  if (!response.ok) throw new Error(data.error || 'Agent 配置请求失败')
  return data
}

export function settingsPayload(settings) {
  return {
    revision: settings.revision,
    codex_api_profile: settings.codex_api_profile || null, claude_api_profile: settings.claude_api_profile || null,
    profiles: settings.profiles.map(profile => ({
      id: profile.id, name: profile.name, base_url: profile.base_url, model: profile.model,
      agent: profile.agent || "shared", protocol: profile.protocol || "responses",
      model_discovery: profile.model_discovery || "auto", reasoning_levels: profile.reasoning_levels || [],
      models: (profile.models || []).map(item => ({ id: item.id, context_length: item.context_length ?? null, context_source: item.context_source ?? null, input_budget: item.input_budget ?? null,
        reasoning_levels: item.reasoning_levels ?? null, default_reasoning_level: item.default_reasoning_level ?? null,
        reasoning_by_protocol: item.reasoning_by_protocol || {} })),
      claude_base_url: profile.claude_base_url?.trim() || null,
      context_tokens: profile.context_tokens === '' || profile.context_tokens == null ? null : Number(profile.context_tokens),
      api_key: profile.clear_key ? '' : profile.api_key || null,
    })),
    firefly: { ...settings.firefly, model: settings.firefly.model || null, reasoning_level: settings.firefly.reasoning_level || null, timeout_secs: Number(settings.firefly.timeout_secs) },
    subagents: settings.subagents.map(agent => ({ ...agent, model: agent.model || null, reasoning_level: agent.reasoning_level || null, timeout_secs: Number(agent.timeout_secs) })),
  }
}

function validationError(settings) {
  for (const profile of settings.profiles) {
    try {
      const url = new URL(profile.base_url)
      if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error()
      if (profile.claude_base_url?.trim()) {
        const claude = new URL(profile.claude_base_url.trim())
        if (claude.origin !== url.origin || claude.username || claude.password || claude.search || claude.hash) return 'Claude 接口地址须与 Base URL 同源，并且不包含凭据或查询参数。'
      }
    } catch { return '请填写有效的模型服务地址，配置会在修正后自动保存。' }
    if (!profile.name?.trim()) return '请填写模型配置名称，配置会在修正后自动保存。'
    if (profile.context_tokens !== '' && profile.context_tokens != null && (!Number.isInteger(Number(profile.context_tokens)) || Number(profile.context_tokens) < 1024 || Number(profile.context_tokens) > 100000000)) return '输入预算须为 1024–100000000，配置会在修正后自动保存。'
  }
  for (const agent of [settings.firefly, ...settings.subagents]) {
    if (!/^[a-z0-9_-]{1,40}$/.test(agent.id)) return 'Agent 标识只能包含小写字母、数字、下划线或连字符。'
    if (!agent.name?.trim()) return '请填写 Agent 名称，配置会在修正后自动保存。'
    if (agent.id !== 'firefly' && agent.enabled && !agent.description?.trim()) return '请填写已启用子 agent 的职责说明，配置会在修正后自动保存。'
    const timeout = Number(agent.timeout_secs)
    if (!Number.isInteger(timeout) || timeout < 1 || timeout > 604800) return '执行期限须为 1–604800 秒，配置会在修正后自动保存。'
  }
  return ''
}

export function useAgentSettings(api = request, { autoSave = true, debounceMs = 900 } = {}) {
  const draft = ref(null), tools = ref([]), loading = ref(false), saving = ref(false), refreshing = ref(false)
  const error = ref(''), notice = ref(''), baseline = ref('')
  let version = 0, timer = null, suppress = false, inFlight = null
  const signature = value => JSON.stringify(settingsPayload(value))
  const dirty = computed(() => !!draft.value && signature(draft.value) !== baseline.value)
  const silent = action => { suppress = true; try { action() } finally { suppress = false } }
  const accept = value => silent(() => {
    draft.value = structuredClone(value)
    baseline.value = signature(draft.value)
  })
  const load = async ({ discardChanges = false } = {}) => {
    if (saving.value || (dirty.value && !discardChanges)) return false
    clearTimeout(timer); timer = null
    const current = ++version
    loading.value = true; error.value = ''; notice.value = ''
    try {
      const [settings, catalog] = await Promise.all([api(), api('/tools')])
      if (current !== version) return false
      accept(settings); tools.value = catalog.tools
      return true
    } catch (failure) { if (current === version) error.value = failure.message; return false }
    finally { if (current === version) loading.value = false }
  }
  const save = async () => {
    clearTimeout(timer); timer = null
    if (inFlight) { if (!await inFlight) return false; return dirty.value ? save() : true }
    if (!draft.value || loading.value || !dirty.value) return !dirty.value
    const problem = validationError(draft.value)
    if (problem) { error.value = problem; return false }
    const snapshot = JSON.parse(JSON.stringify(settingsPayload(draft.value)))
    const current = ++version
    saving.value = true; error.value = ''; notice.value = ''
    const pending = (async () => {
      try {
        const value = await api('', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(snapshot) })
        if (current !== version) return false
        silent(() => {
          // Preserve object identity for in-flight discovery and keep newer edits.
          draft.value.revision = value.revision
          for (const saved of value.profiles) {
            const live = draft.value.profiles.find(item => item.id === saved.id)
            const sent = snapshot.profiles.find(item => item.id === saved.id)
            if (!live || !sent) continue
            const liveKey = live.clear_key ? '' : live.api_key || null
            if (liveKey === sent.api_key && live.base_url === sent.base_url) {
              live.api_key = null; delete live.clear_key; live.key_configured = saved.key_configured
            }
          }
          baseline.value = signature(value)
        })
        notice.value = '已自动保存'
        return true
      } catch (failure) { if (current === version) error.value = failure.message; return false }
      finally { if (current === version) saving.value = false }
    })()
    inFlight = pending
    const ok = await pending
    if (inFlight === pending) inFlight = null
    if (ok && dirty.value) return save()
    return ok
  }
  if (autoSave) watch(draft, () => {
    if (suppress || loading.value || !draft.value || !dirty.value) return
    error.value = ''; notice.value = ''
    clearTimeout(timer)
    timer = setTimeout(() => { void save() }, debounceMs)
  }, { deep: true, flush: 'sync' })
  const refreshTools = async () => {
    if (refreshing.value || loading.value) return
    refreshing.value = true
    try { tools.value = (await api('/tools')).tools }
    catch (failure) { error.value = failure.message }
    finally { refreshing.value = false }
  }
  if (getCurrentScope()) onScopeDispose(() => clearTimeout(timer))
  return { draft, tools, loading, saving, refreshing, error, notice, dirty, load, save, refreshTools }
}
