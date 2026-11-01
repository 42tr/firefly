import { computed, ref } from 'vue'
import { apiFetch } from '@/utils/http'

async function request(path = '', options = {}) {
  const response = await apiFetch('/api/assistant/agent-settings' + path, options)
  const data = await response.json()
  if (!response.ok) throw new Error(data.error || 'Agent 配置请求失败')
  return data
}

export function settingsPayload(settings) {
  return {
    revision: settings.revision,
    profiles: settings.profiles.map(profile => ({
      id: profile.id, name: profile.name, agent: profile.agent || 'firefly', protocol: profile.protocol || 'responses',
      base_url: profile.base_url, model: profile.model, models: Array.isArray(profile.models) ? profile.models : [],
      reasoning_levels: Array.isArray(profile.reasoning_levels) ? profile.reasoning_levels : [],
      model_discovery: profile.model_discovery || 'auto',
      context_tokens: profile.context_tokens === '' || profile.context_tokens == null ? null : Number(profile.context_tokens),
      api_key: profile.clear_key ? '' : profile.api_key || null,
    })),
    firefly: { ...settings.firefly, timeout_secs: Number(settings.firefly.timeout_secs) },
    subagents: settings.subagents.map(agent => ({ ...agent, timeout_secs: Number(agent.timeout_secs) })),
    codex_api_profile: settings.codex_api_profile || null,
    claude_api_profile: settings.claude_api_profile || null,
  }
}

export function portableSettings(settings) {
  const value = settingsPayload(settings)
  value.profiles.forEach(profile => { profile.api_key = null })
  return { format_version: 1, profiles: value.profiles, firefly: value.firefly, subagents: value.subagents }
}

export function useAgentSettings(api = request) {
  const draft = ref(null), tools = ref([]), loading = ref(false), saving = ref(false), refreshing = ref(false)
  const error = ref(''), notice = ref(''), baseline = ref('')
  let version = 0
  const dirty = computed(() => draft.value && JSON.stringify(draft.value) !== baseline.value)
  const accept = value => {
    draft.value = structuredClone(value)
    baseline.value = JSON.stringify(draft.value)
  }
  const load = async () => {
    if (saving.value) return false
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
    if (!draft.value || saving.value || loading.value) return false
    const current = ++version
    saving.value = true; error.value = ''; notice.value = ''
    try {
      const value = await api('', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(settingsPayload(draft.value)) })
      if (current !== version) return false
      accept(value); notice.value = '已保存，从下一轮对话生效。'
      return true
    } catch (failure) { if (current === version) error.value = failure.message; return false }
    finally { if (current === version) saving.value = false }
  }
  const refreshTools = async () => {
    if (refreshing.value || loading.value || saving.value) return
    refreshing.value = true
    try { tools.value = (await api('/tools')).tools }
    catch (failure) { error.value = failure.message }
    finally { refreshing.value = false }
  }
  const importConfig = value => {
    if (!draft.value) throw new Error('请先读取当前配置')
    if (value.format_version !== 1 || !Array.isArray(value.profiles) || !Array.isArray(value.subagents) || !value.firefly) {
      throw new Error('请选择 Firefly 导出的 Agent 配置文件')
    }
    const agentShape = item => item && typeof item.id === 'string' && typeof item.name === 'string'
      && typeof item.description === 'string' && typeof item.instruction === 'string'
      && Array.isArray(item.tools) && item.tools.every(tool => typeof tool === 'string')
      && typeof item.enabled === 'boolean' && Number.isFinite(Number(item.timeout_secs))
      && (item.model_profile == null || typeof item.model_profile === 'string')
    const profileShape = item => item && ['id', 'name', 'base_url', 'model'].every(key => typeof item[key] === 'string')
      && (item.agent == null || typeof item.agent === 'string')
      && (item.protocol == null || typeof item.protocol === 'string')
      && (item.models == null || Array.isArray(item.models))
      && (item.reasoning_levels == null || Array.isArray(item.reasoning_levels))
      && (item.model_discovery == null || typeof item.model_discovery === 'string')
      && (item.api_key == null || typeof item.api_key === 'string')
    if (!agentShape(value.firefly) || !value.subagents.every(agentShape) || !value.profiles.every(profileShape)) {
      throw new Error('配置文件中的 Agent 或模型字段不完整')
    }
    const payload = settingsPayload({ ...value, revision: draft.value.revision })
    draft.value = payload
    notice.value = '配置已导入草稿，检查后保存即可生效。'; error.value = ''
  }
  return { draft, tools, loading, saving, refreshing, error, notice, dirty, load, save, refreshTools, importConfig }
}
