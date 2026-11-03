<script setup>
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import UiIcon from './UiIcon.vue'
import { useAgentSettings } from '../composables/useAgentSettings'
import { apiFetch } from '../utils/http'

const props = defineProps({
  active: { type: Boolean, default: true },
  showTrigger: { type: Boolean, default: true },
})
const emit = defineEmits(['closed'])
const { draft, tools, loading, saving, refreshing, error, notice, dirty, load, save, refreshTools } = useAgentSettings()
const dialog = ref(null), opened = ref(false), tab = ref('firefly'), selectedAgent = ref(''), selectedProfile = ref('')
const busy = computed(() => loading.value)
const agent = computed(() => !draft.value ? null : tab.value === 'firefly' ? draft.value.firefly : draft.value.subagents.find(item => item.id === selectedAgent.value))
const profile = computed(() => draft.value?.profiles.find(item => item.id === selectedProfile.value))
const selectableProfiles = runtime => (draft.value?.profiles || []).filter(item => {
  const scope = item.agent || 'shared', protocol = item.protocol || 'responses'
  return (scope === runtime || scope === 'shared') && (runtime === 'claude'
    ? protocol === 'anthropic' || scope === 'shared' : runtime === 'firefly' ? protocol === 'responses' : ['responses', 'openai'].includes(protocol))
})
const agentProfile = computed(() => draft.value?.profiles.find(item => item.id === (agent.value?.model_profile || draft.value?.firefly.model_profile)))
const agentModels = computed(() => {
  const values = [...(agentProfile.value?.models || [])]
  const fallback = agentProfile.value?.model
  if (fallback && !values.some(item => item.id === fallback)) values.push({ id: fallback })
  return values
})
const inheritedModel = computed(() => (tab.value === 'children' && !agent.value?.model_profile ? draft.value?.firefly.model : null)
  || agentProfile.value?.model || agentModels.value[0]?.id || '')
const agentModel = computed({
  get: () => agent.value?.model || inheritedModel.value,
  set: value => { if (agent.value) agent.value.model = value || null },
})
const agentModelInfo = computed(() => agentModels.value.find(item => item.id === agentModel.value))
const agentReasoningInfo = computed(() => agentModelInfo.value?.reasoning_by_protocol?.[agentProfile.value?.protocol || 'responses'] || agentModelInfo.value)
const agentReasoningLevels = computed(() => agentReasoningInfo.value?.reasoning_levels || [])
const inheritsRootReasoning = computed(() => {
  const root = draft.value?.firefly
  const rootProfile = draft.value?.profiles.find(item => item.id === root?.model_profile)
  const rootModel = root?.model || rootProfile?.model || rootProfile?.models?.[0]?.id
  return tab.value === 'children' && !!agentProfile.value && agentProfile.value.id === rootProfile?.id && agentModel.value === rootModel
})
const inheritedReasoningLevel = computed(() => {
  const levels = agentReasoningLevels.value
  const inherited = inheritsRootReasoning.value ? draft.value?.firefly.reasoning_level : null
  return [inherited, agentReasoningInfo.value?.default_reasoning_level].find(level => levels.includes(level)) || levels[0] || ''
})
const agentReasoningLevel = computed({
  get: () => agentReasoningLevels.value.includes(agent.value?.reasoning_level)
    ? agent.value.reasoning_level : inheritedReasoningLevel.value,
  set: value => { if (agent.value) agent.value.reasoning_level = value || null },
})
const agentModelChanged = () => { agent.value.reasoning_level = null }
const agentProfileChanged = () => { agent.value.model = null; agentModelChanged() }
watch([agent, agentModel, agentReasoningLevels], () => {
  if (tab.value !== 'models' && agent.value?.reasoning_level && !agentReasoningLevels.value.includes(agent.value.reasoning_level)) {
    agent.value.reasoning_level = null
  }
})
const missingTools = computed(() => agent.value?.tools.filter(name => !tools.value.some(tool => tool.name === name)) || [])
const catalog = ref({}), catalogLoading = ref(false), catalogError = ref('')
let catalogVersion = 0
const models = computed(() => catalog.value[selectedProfile.value] || profile.value?.models || [])
const selectedModel = computed(() => models.value.find(item => item.id === profile.value?.model))
const contextSource = computed(() => {
  try {
    const url = new URL(selectedModel.value?.context_source)
    return ['http:', 'https:'].includes(url.protocol) ? url.href : ''
  } catch { return '' }
})
const applyModel = () => {
  if (!profile.value) return
  profile.value.context_tokens = selectedModel.value?.input_budget ?? null
}
const discoverModels = async () => {
  const currentProfile = profile.value
  if (!currentProfile?.base_url || catalogLoading.value) return
  const current = ++catalogVersion
  catalogLoading.value = true; catalogError.value = ''
  try {
    const response = await apiFetch('/api/assistant/agent-settings/models', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ base_url: currentProfile.base_url, profile_id: currentProfile.id,
        api_key: currentProfile.clear_key ? '' : currentProfile.api_key || null }),
    })
    const data = await response.json().catch(() => ({}))
    if (!response.ok) throw new Error(data.error || '无法读取模型列表')
    if (current !== catalogVersion || profile.value !== currentProfile) return
    catalog.value = { ...catalog.value, [currentProfile.id]: data.models }
    currentProfile.models = data.models
    if (data.models.length && !data.models.some(item => item.id === currentProfile.model)) {
      currentProfile.model = data.models[0].id
      applyModel()
      if (currentProfile.name === '新模型配置') {
        try { currentProfile.name = new URL(currentProfile.base_url).hostname } catch { /* URL validation happens on save. */ }
      }
    } else if (currentProfile.context_tokens == null && selectedModel.value?.input_budget != null) {
      applyModel()
    }
  } catch (failure) { if (current === catalogVersion) catalogError.value = failure.message || '无法读取模型列表' }
  finally { if (current === catalogVersion) catalogLoading.value = false }
}
const connectionChanged = () => {
  if (!profile.value) return
  catalogVersion++; catalogLoading.value = false; catalogError.value = ''
  catalog.value = { ...catalog.value, [profile.value.id]: [] }
  profile.value.models = []
  profile.value.context_tokens = null
  if (profile.value.base_url) void discoverModels()
}
watch([tab, selectedProfile, opened], () => {
  catalogVersion++; catalogLoading.value = false; catalogError.value = ''
  if (opened.value && tab.value === 'models' && profile.value && !catalog.value[profile.value.id]
      && (profile.value.key_configured || profile.value.model)) void discoverModels()
})
const toolQuery = ref('')
const filteredTools = computed(() => {
  const query = toolQuery.value.trim().toLowerCase()
  return query ? tools.value.filter(tool => tool.name.toLowerCase().includes(query) || tool.description?.toLowerCase().includes(query)) : tools.value
})
const newId = prefix => prefix + '-' + Date.now().toString(36) + Math.random().toString(36).slice(2, 6)
const status = child => {
  if (!child.enabled) return '已停用'
  const modelId = child.model_profile || draft.value?.firefly.model_profile
  if (!child.model && !draft.value?.profiles.find(item => item.id === modelId)?.model?.trim()
      && !draft.value?.profiles.find(item => item.id === modelId)?.models?.length) return '待配置模型'
  if (child.tools.some(name => !tools.value.some(tool => tool.name === name))) return '工具不可用'
  return '已启用'
}
const statusKind = child => {
  if (!child.enabled) return 'idle'
  const modelId = child.model_profile || draft.value?.firefly.model_profile
  if (!child.model && !draft.value?.profiles.find(item => item.id === modelId)?.model?.trim()
      && !draft.value?.profiles.find(item => item.id === modelId)?.models?.length) return 'warning'
  if (child.tools.some(name => !tools.value.some(tool => tool.name === name))) return 'error'
  return 'success'
}
const profileKind = item => item.model?.trim() || item.models?.length ? 'success' : 'warning'
const show = async () => {
  catalog.value = {}; catalogError.value = ''
  opened.value = true
  await nextTick(); dialog.value?.showModal()
  if (await load()) {
    selectedAgent.value = draft.value.subagents[0]?.id || ''
    selectedProfile.value = draft.value.profiles[0]?.id || ''
  }
}
defineExpose({ show })
const close = async () => {
  if (dialog.value?.contains(document.activeElement)) document.activeElement?.blur()
  if (!await save()) return
  dialog.value?.close(); opened.value = false; emit('closed')
}
const addAgent = () => {
  const id = newId('agent')
  draft.value.subagents.push({ id, name: '新子 agent', description: '', instruction: '', model_profile: null, model: null, reasoning_level: null,
    tools: [], enabled: false, timeout_secs: 1800 })
  selectedAgent.value = id
}
const removeAgent = () => {
  draft.value.subagents = draft.value.subagents.filter(item => item !== agent.value)
  selectedAgent.value = draft.value.subagents[0]?.id || ''
}
const addProfile = () => {
  const id = newId('model')
  draft.value.profiles.push({ id, name: '新模型配置', agent: 'shared', protocol: 'responses', model_discovery: 'auto', base_url: 'https://api.openai.com/v1', model: '',
    models: [], api_key: null, key_configured: false, context_tokens: null })
  selectedProfile.value = id
  if (!draft.value.firefly.model_profile) draft.value.firefly.model_profile = id
}
const removeProfile = () => {
  const id = selectedProfile.value
  const rootAffected = draft.value.firefly.model_profile === id
  const affected = draft.value.subagents.filter(item => item.model_profile === id)
  if (rootAffected) { draft.value.firefly.model_profile = null; draft.value.firefly.model = null; draft.value.firefly.reasoning_level = null }
  for (const child of affected) { child.model_profile = null; child.model = null; child.reasoning_level = null }
  if (draft.value.codex_api_profile === id) draft.value.codex_api_profile = null
  if (draft.value.claude_api_profile === id) draft.value.claude_api_profile = null
  draft.value.profiles = draft.value.profiles.filter(item => item.id !== id)
  selectedProfile.value = draft.value.profiles[0]?.id || ''
  error.value = ''
  notice.value = rootAffected || affected.length
    ? '已移除模型配置：受影响的子 agent 将继承主模型；如主 agent 使用此配置，请重新选择模型。'
    : '已移除模型配置。'
}
const renameAgentId = event => {
  if (event.target.value === 'firefly' || draft.value.subagents.some(item => item !== agent.value && item.id === event.target.value)) {
    error.value = '此 Agent 标识已存在，请使用不同标识。'
    event.target.value = agent.value.id
    return
  }
  agent.value.id = event.target.value
  selectedAgent.value = event.target.value
}
watch(() => props.active, active => { if (!active && opened.value) close() })
onBeforeUnmount(() => dialog.value?.close())
</script>

<template>
  <button v-if="showTrigger" class="button ghost small" type="button" role="menuitem" aria-haspopup="dialog" :aria-expanded="opened" aria-controls="agent-settings-dialog" @click="show"><UiIcon name="sliders" /><span>Agent 配置</span></button>
  <Teleport to="body">
    <dialog id="agent-settings-dialog" ref="dialog" class="agent-settings-dialog" aria-labelledby="agent-settings-title" @keydown.esc.stop @cancel.prevent="close" @click.self="close">
      <header class="settings-header">
        <div class="header-title">
          <span class="header-icon" aria-hidden="true"><UiIcon name="sliders" /></span>
          <div class="header-text">
            <h2 id="agent-settings-title">Agent 配置</h2>
            <p>配置主助手，按需创建子 agent 并分配工具。</p>
          </div>
        </div>
        <div class="header-actions"><span class="save-status" role="status">{{ saving ? '正在保存…' : error && dirty ? '保存失败' : dirty ? '等待自动保存' : notice || '自动保存已开启' }}</span><button class="button ghost small" type="button" @click="close">关闭</button></div>
      </header>
      <div class="settings-content">
        <p v-if="loading" role="status">正在读取配置…</p>
        <template v-if="draft">
          <nav class="settings-tabs" aria-label="配置分类">
            <button v-for="entry in [['firefly', 'Firefly 主 agent'], ['children', '子 agent'], ['models', '模型配置']]" :key="entry[0]" class="tab-button" :class="{ active: tab === entry[0] }" type="button" :aria-pressed="tab === entry[0]" :disabled="busy" @click="tab = entry[0]">{{ entry[1] }}<span v-if="entry[0] === 'children'" class="count">{{ draft.subagents.length }}</span></button>
          </nav>
          <form class="settings-form" @submit.prevent>
            <fieldset :disabled="busy" class="settings-fields">
              <div class="settings-layout" :class="{ 'root-layout': tab === 'firefly' }">
                <aside v-if="tab === 'children'" class="settings-sidebar" aria-label="子 agent 列表">
                  <button class="add-entry" type="button" :disabled="draft.subagents.length >= 50" @click="addAgent">＋ 新建子 agent</button>
                  <button v-for="child in draft.subagents" :key="child.id" class="entry-card" type="button" :aria-pressed="selectedAgent === child.id" @click="selectedAgent = child.id"><strong>{{ child.name }}</strong><span class="entry-status" :data-kind="statusKind(child)"><i aria-hidden="true"></i>{{ status(child) }}</span></button>
                  <p v-if="!draft.subagents.length" class="empty-hint">尚未创建子 agent。主助手可以独立工作。</p>
                </aside>
                <aside v-if="tab === 'models'" class="settings-sidebar" aria-label="模型配置列表">
                  <button class="add-entry" type="button" :disabled="draft.profiles.length >= 32" @click="addProfile">＋ 新建模型配置</button>
                  <button v-for="item in draft.profiles" :key="item.id" class="entry-card" type="button" :aria-pressed="selectedProfile === item.id" @click="selectedProfile = item.id"><strong>{{ item.name }}</strong><span class="entry-status" :data-kind="profileKind(item)"><i aria-hidden="true"></i>{{ item.model || '待配置模型' }}</span></button>
                </aside>
                <section v-if="agent && tab !== 'models'" class="settings-editor" :aria-label="tab === 'firefly' ? 'Firefly 主 agent 设置' : '子 agent 设置'">
                  <div class="editor-heading"><h3>{{ tab === 'firefly' ? 'Firefly 主 agent' : agent.name }}</h3><button v-if="tab === 'children'" class="delete-entry" type="button" @click="removeAgent">删除子 agent</button></div>
                  <div class="section-card">
                    <div class="section-title"><h4>基本信息</h4><label v-if="tab === 'children'" class="check"><input v-model="agent.enabled" type="checkbox" /><span>启用子 agent</span></label></div>
                    <div class="field-pair">
                      <label>名称<input v-model="agent.name" maxlength="100" required /></label>
                      <label v-if="tab === 'children'">标识<input :value="agent.id" aria-label="标识" maxlength="40" pattern="[a-z0-9_-]+" required @input="renameAgentId" /><small>小写字母、数字、下划线或连字符。</small></label>
                    </div>
                    <label v-if="tab === 'children'">职责说明<textarea v-model="agent.description" rows="2" maxlength="4096" :required="agent.enabled" placeholder="主助手根据这段说明选择何时委派任务。"></textarea></label>
                  </div>
                  <div class="section-card">
                    <div class="section-title"><h4>模型与执行</h4></div>
                    <div class="field-pair">
                      <label>模型平台<select v-model="agent.model_profile" @change="agentProfileChanged"><option :value="null">{{ tab === 'children' ? '继承 Firefly 的平台' : '请选择模型平台' }}</option><option v-for="item in selectableProfiles('firefly')" :key="item.id" :value="item.id">{{ item.name }}</option></select></label>
                      <label>执行期限（秒）<input v-model.number="agent.timeout_secs" type="number" min="1" max="604800" required /></label>
                    </div>
                    <label>使用模型<select v-model="agentModel" aria-label="Agent 使用模型" :disabled="!agentProfile" @change="agentModelChanged"><option v-if="!agentModel" value="" disabled>请选择模型</option><option v-if="agentModel && !agentModels.some(item => item.id === agentModel)" :value="agentModel">{{ agentModel }}</option><option v-for="item in agentModels" :key="item.id" :value="item.id">{{ item.id }}{{ item.context_length ? ' · ' + item.context_length.toLocaleString() + ' tokens' : '' }}</option></select></label>
                    <label>推理级别<select v-model="agentReasoningLevel" aria-label="Agent 推理级别" :disabled="!agentReasoningLevels.length">
                      <option v-if="!agentReasoningLevels.length" value="">使用服务默认设置</option>
                      <option v-for="level in agentReasoningLevels" :key="level" :value="level">{{ level }}</option>
                    </select><small>{{ agentReasoningLevels.length ? '可选级别依据模型接口或官方文档，修改后自动保存，从下一次运行开始使用。' : '未获取到该模型的可选推理级别，使用服务端默认设置。' }}</small></label>
                  </div>
                  <div class="section-card">
                    <label class="prompt-label">系统提示词<textarea v-model="agent.instruction" class="prompt-input" rows="8" maxlength="131072" placeholder="填写角色、工作流程、约束和返回要求。"></textarea></label>
                  </div>
                  <div class="section-card">
                    <div class="tools-heading"><h4>可用工具<span class="tool-count">{{ agent.tools.length }} 项已选</span></h4><div class="tools-actions"><input v-model="toolQuery" class="tool-filter" type="search" placeholder="筛选工具" aria-label="筛选工具" @keydown.enter.prevent /><button class="button ghost small" type="button" :disabled="refreshing" @click="refreshTools">{{ refreshing ? '读取中…' : '刷新工具' }}</button></div></div>
                    <p class="field-hint">勾选要分配给此 agent 的工具。扩展工具需先在“扩展模块”中启用。</p>
                    <div class="tool-list">
                      <label v-for="tool in filteredTools" :key="tool.name" class="tool-option"><input v-model="agent.tools" :value="tool.name" type="checkbox" /><span><strong>{{ tool.name }}</strong><small>{{ tool.source }} · {{ tool.read_only ? '只读' : '可执行操作' }}</small><span class="tool-description">{{ tool.description }}</span></span></label>
                      <label v-for="name in missingTools" :key="name" class="tool-option unavailable-tool"><input v-model="agent.tools" :value="name" type="checkbox" /><span><strong>{{ name }}</strong><small>工具不可用，请启用对应模块或取消选择。</small></span></label>
                      <p v-if="!filteredTools.length && !missingTools.length" class="tools-empty">{{ toolQuery.trim() ? '没有匹配的工具。' : '暂无可用工具。' }}</p>
                    </div>
                  </div>
                </section>
                <section v-else-if="tab === 'models' && profile" class="settings-editor" aria-label="模型配置设置">
                  <div class="editor-heading"><h3>{{ profile.name }}</h3><button class="delete-entry" type="button" @click="removeProfile">删除模型配置</button></div>
                  <div class="section-card">
                    <div class="section-title"><h4>连接</h4></div>
                    <label>Base URL<input v-model.trim.lazy="profile.base_url" aria-label="服务地址" type="url" maxlength="2048" required placeholder="https://api.openai.com/v1" @change="connectionChanged" /><small>使用支持 Responses API 的模型服务。模型列表会先查询此地址下的 /models，再尝试同一服务的 /v1/models。</small></label>
                  </div>
                  <div class="section-card">
                    <div class="section-title"><h4>认证</h4></div>
                    <label>API Key<input v-model.lazy="profile.api_key" aria-label="API Key" type="password" maxlength="16384" autocomplete="new-password" :placeholder="profile.key_configured ? '已保存，同一服务下留空保留' : '填写密钥；无认证服务可留空'" @change="profile.clear_key = false; connectionChanged()" /><small>更换协议、域名或端口后，留空保存会清除原密钥。</small></label>
                    <div class="key-controls"><span>{{ profile.clear_key ? '正在清除密钥' : profile.key_configured ? '已有密钥' : '尚未保存密钥' }}</span><button class="button ghost small" type="button" @click="profile.api_key = null; profile.clear_key = !profile.clear_key; connectionChanged()">{{ profile.clear_key ? '保留原密钥' : '清除密钥' }}</button></div>
                  </div>
                  <div class="section-card">
                    <div class="section-title"><h4>可用模型</h4><button class="button ghost small" type="button" :disabled="catalogLoading || !profile.base_url" @click="discoverModels">{{ catalogLoading ? '正在获取…' : '获取模型' }}</button></div>
                    <p class="field-hint">获取到的全部模型会自动保存，供各 Agent 和对话分别选择。这里选择平台默认模型。</p>
                    <p v-if="catalogError" class="settings-error" role="alert">{{ catalogError }}</p>
                    <label>默认模型<select v-model="profile.model" aria-label="模型名称" @change="applyModel"><option value="">请选择默认模型</option><option v-if="profile.model && !selectedModel" :value="profile.model">{{ profile.model }} · 已保存</option><option v-for="item in models" :key="item.id" :value="item.id">{{ item.id }} · {{ item.context_length ? item.context_length.toLocaleString() + ' tokens' : '上下文长度未知' }}</option></select></label>
                    <p v-if="selectedModel && !selectedModel.context_length" class="field-hint">模型服务及官方资料均未提供可用的上下文长度。图像、音频、视频生成和自动路由模型可能没有固定的文本上下文窗口；可在下方高级设置填写输入预算。</p>
                    <p v-else-if="selectedModel?.context_length" class="field-hint">上下文 {{ selectedModel.context_length.toLocaleString() }} tokens<span v-if="selectedModel.input_budget"> · 输入预算 {{ selectedModel.input_budget.toLocaleString() }} tokens（已预留输出空间）</span><span v-else> · 无法计算输入预算，请检查模型信息。</span></p>
                    <p v-if="contextSource" class="field-hint">上下文来源：<a :href="contextSource" target="_blank" rel="noopener noreferrer">{{ contextSource.includes('help.aliyun.com/') ? '阿里云官方文档' : contextSource.includes('qianwenai.com/models/') ? '千问模型详情' : '模型元数据' }}</a></p>
                    <details><summary>高级设置</summary><label>可用范围<select v-model="profile.agent"><option value="shared">共享</option><option value="firefly">Firefly</option><option value="codex">Codex</option><option value="claude">Claude</option></select></label><label>接口协议<select v-model="profile.protocol"><option value="responses">Responses</option><option value="anthropic">Anthropic Messages</option><option value="openai">OpenAI 兼容</option></select></label><label>Codex 默认平台<select v-model="draft.codex_api_profile"><option :value="null">沿用 CLI 配置</option><option v-for="item in selectableProfiles('codex')" :key="item.id" :value="item.id">{{ item.name }}</option></select></label><label>Claude 默认平台<select v-model="draft.claude_api_profile"><option :value="null">沿用 CLI 配置</option><option v-for="item in selectableProfiles('claude')" :key="item.id" :value="item.id">{{ item.name }}</option></select></label><label>配置名称<input v-model="profile.name" maxlength="100" required /></label><label>Claude 接口地址（可选）<input v-model.trim.lazy="profile.claude_base_url" type="url" placeholder="默认从 Base URL 移除末尾 /v1" /><small>Claude 需要 Anthropic Messages 接口；可填写同一服务下的独立路径。</small></label><label>输入预算<input v-model.number="profile.context_tokens" aria-label="输入预算" type="number" min="1024" max="100000000" placeholder="自动获取；服务未提供时可手动填写" /><small>仅在服务未提供完整信息或需要自定义上限时使用。</small></label><label v-if="!models.length">模型名称<input v-model.trim="profile.model" maxlength="256" placeholder="模型列表不可用时手动填写" /></label></details>
                  </div>
                </section>
                <section v-else-if="tab === 'children' || tab === 'models'" class="settings-editor"><div class="empty-editor">{{ tab === 'children' ? '从左侧选择一个子 agent，或点击“＋ 新建子 agent”开始配置。' : '从左侧选择一个模型配置，或点击“＋ 新建模型配置”。' }}</div></section>
              </div>
            </fieldset>
          </form>
        </template>
        <p v-if="error" class="settings-error" role="alert">{{ error }}<button v-if="!draft" class="button ghost small" type="button" :disabled="loading" @click="show">重新读取</button><button v-else-if="error.includes('其他页面修改')" class="button ghost small" type="button" :disabled="saving" @click="load({ discardChanges: true })">放弃本地修改并重新读取</button><button v-else class="button ghost small" type="button" :disabled="saving" @click="save">重试</button></p>
      </div>

    </dialog>
  </Teleport>
</template>

<style scoped>
.agent-settings-dialog { width: min(980px, calc(100vw - 32px)); height: min(880px, calc(100dvh - 40px)); max-width: none; padding: 0; overflow: hidden; border: 1px solid var(--border-glass); border-radius: var(--radius-lg); color: var(--text-primary); background: var(--bg-panel); box-shadow: var(--shadow-dialog); }
.agent-settings-dialog[open] { display: flex; flex-direction: column; }

/* ---------- 头部：品牌图标 + 标题 ---------- */
.settings-header { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-shrink: 0; padding: 18px 24px; border-bottom: 1px solid var(--border-soft); }
.header-title { display: flex; align-items: center; gap: 13px; min-width: 0; }
.header-icon { display: grid; place-items: center; width: 40px; height: 40px; flex-shrink: 0; border-radius: 12px; background: var(--ink-gradient); color: var(--glow); box-shadow: 0 6px 16px -6px rgba(23, 32, 51, .5); }
.settings-header h2 { margin: 0; font-size: 17px; font-weight: 600; }
.settings-header p { margin: 3px 0 0; color: var(--muted); font-size: 12px; line-height: 1.5; }
.header-actions { display: flex; align-items: center; gap: 12px; }.save-status { color: var(--muted); font-size: 12px; }

.settings-content { display: flex; flex: 1; flex-direction: column; padding: 20px 24px 24px; overflow-y: auto; min-height: 0; background: var(--bg-body); }

/* ---------- 分段标签 ---------- */
.settings-tabs { display: flex; flex-shrink: 0; gap: 4px; width: fit-content; max-width: 100%; margin-bottom: 18px; padding: 4px; border: 1px solid var(--border-soft); border-radius: 999px; background: var(--bg-subtle); }
.tab-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 32px; padding: 5px 15px; border: 0; border-radius: 999px; background: transparent; color: var(--muted); font: inherit; font-size: 13px; font-weight: 500; white-space: nowrap; cursor: pointer; transition: background .18s var(--ease-out), color .18s var(--ease-out), box-shadow .18s var(--ease-out); }
.tab-button:hover:not(:disabled):not(.active) { color: var(--text-primary); }
.tab-button.active { background: var(--bg-panel); color: var(--text-primary); box-shadow: var(--shadow-xs); }
.tab-button:disabled { cursor: not-allowed; opacity: .55; }
.count { min-width: 18px; padding: 0 6px; border-radius: 999px; background: var(--bg-hover); color: var(--muted); font-size: 11px; line-height: 18px; text-align: center; }
.tab-button.active .count { background: var(--accent-soft); color: var(--accent-strong); }

/* ---------- 布局与侧栏：左右两列各自独立滚动 ---------- */
/* 空间充足时布局填满、左右两列各自滚动；空间不足时 form/fieldset 不可压缩
   （flex-shrink: 0），配合布局的 360px 地板，超出部分由 .settings-content
   整体滚动兜底，编辑区与提示条始终完整可见、互不遮挡。 */
.settings-form { display: flex; flex: 1 0 auto; flex-direction: column; min-height: 0; }
.settings-fields { display: flex; flex: 1 0 auto; flex-direction: column; min-width: 0; min-height: 0; border: 0; padding: 0; margin: 0; transition: opacity .2s var(--ease-out); }
.settings-fields:disabled { opacity: .6; }
.settings-layout { display: grid; flex: 1; min-height: 360px; grid-template-columns: 216px minmax(0, 1fr); grid-template-rows: minmax(0, 1fr); gap: 22px; overflow: hidden; }
.root-layout { grid-template-columns: minmax(0, 1fr); }
.settings-sidebar { display: flex; flex-direction: column; align-items: stretch; gap: 8px; min-width: 0; min-height: 0; padding: 2px; overflow-y: auto; }
.add-entry { position: sticky; top: 0; left: 0; z-index: 1; display: flex; flex-shrink: 0; align-items: center; justify-content: center; gap: 6px; width: 100%; min-height: 38px; padding: 8px 12px; border: 1px dashed var(--accent-border); border-radius: 12px; background: var(--bg-body); color: var(--muted); font: inherit; font-size: 13px; cursor: pointer; transition: border-color .18s var(--ease-out), color .18s var(--ease-out), background .18s var(--ease-out); }
.add-entry:hover:not(:disabled) { border-color: var(--accent); background: var(--accent-soft); color: var(--accent-strong); }
.add-entry:disabled { cursor: not-allowed; opacity: .5; }
.entry-card { display: flex; flex-direction: column; align-items: flex-start; gap: 6px; width: 100%; padding: 11px 12px; border: 1px solid var(--border-soft); border-radius: 12px; background: var(--bg-panel); color: var(--text-primary); font: inherit; cursor: pointer; overflow-wrap: anywhere; text-align: left; transition: border-color .18s var(--ease-out), background .18s var(--ease-out), box-shadow .18s var(--ease-out), transform .18s var(--ease-out); }
.entry-card:hover { border-color: var(--accent-border); box-shadow: var(--shadow-xs); transform: translateY(-1px); }
.entry-card[aria-pressed="true"] { border-color: var(--accent); background: var(--accent-soft); box-shadow: var(--shadow-xs); }
.entry-card strong { font-size: 13px; font-weight: 600; line-height: 1.4; }
.entry-status { display: inline-flex; align-items: center; gap: 6px; color: var(--muted); font-size: 12px; line-height: 1.5; }
.entry-status i { width: 7px; height: 7px; flex-shrink: 0; border-radius: 50%; background: var(--status-idle); }
.entry-status[data-kind="success"] { color: var(--success-text); }.entry-status[data-kind="success"] i { background: var(--success-text); box-shadow: 0 0 0 3px rgba(21, 128, 61, .12); }
.entry-status[data-kind="warning"] { color: var(--warning-text); }.entry-status[data-kind="warning"] i { background: var(--warning-text); box-shadow: 0 0 0 3px rgba(161, 98, 7, .12); }
.entry-status[data-kind="error"] { color: var(--error-text); }.entry-status[data-kind="error"] i { background: var(--error-text); box-shadow: 0 0 0 3px rgba(185, 28, 28, .12); }
.empty-hint, .field-hint { margin: 0; font-size: 12px; color: var(--muted); line-height: 1.7; }
.empty-editor { display: grid; flex: 1; place-items: center; padding: 32px; border: 1px dashed var(--accent-border); border-radius: var(--radius); color: var(--muted); font-size: 13px; line-height: 1.8; text-align: center; }

/* ---------- 编辑区与分组卡片 ---------- */
.settings-editor { display: flex; flex-direction: column; gap: 16px; min-width: 0; min-height: 0; padding: 0 2px; overflow-y: auto; }
.editor-heading { position: sticky; top: 0; z-index: 2; display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 4px 2px 10px; background: var(--bg-body); box-shadow: 0 1px 0 var(--border-soft); }
.editor-heading h3 { margin: 0; font-size: 16px; font-weight: 600; }
.delete-entry { min-height: 32px; padding: 5px 13px; border: 1px solid transparent; border-radius: 999px; background: transparent; color: var(--danger); font: inherit; font-size: 13px; cursor: pointer; transition: background .18s var(--ease-out), border-color .18s var(--ease-out); }
.delete-entry:hover { background: rgba(220, 38, 38, .08); border-color: rgba(220, 38, 38, .22); }
.section-card { display: flex; flex-direction: column; gap: 14px; padding: 18px; border: 1px solid var(--border-soft); border-radius: var(--radius); background: var(--bg-panel); box-shadow: var(--shadow-xs); }
.section-title { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.section-title h4, .tools-heading h4 { margin: 0; font-size: 13px; font-weight: 600; color: var(--text-secondary); letter-spacing: .02em; }
.tools-heading { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 10px; }
.tools-actions { display: flex; align-items: center; gap: 8px; }
.settings-editor input.tool-filter { width: 150px; padding: 6px 10px; font-size: 12px; }
.tool-count { margin-left: 8px; color: var(--muted); font-weight: 400; }

/* ---------- 表单控件 ---------- */
.settings-editor label { display: flex; flex-direction: column; gap: 7px; font-size: 13px; font-weight: 500; color: var(--text-secondary); }
.settings-editor small { color: var(--muted); line-height: 1.6; font-size: 12px; font-weight: 400; }
.field-pair { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; }
input:not([type="checkbox"]), select, textarea { width: 100%; box-sizing: border-box; min-width: 0; padding: 9px 12px; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); color: var(--text-primary); background: var(--bg-subtle); font: inherit; font-size: 13px; font-weight: 400; transition: border-color .18s var(--ease-out), box-shadow .18s var(--ease-out); }
input:not([type="checkbox"]):hover:not(:disabled), select:hover:not(:disabled), textarea:hover:not(:disabled) { border-color: var(--accent-border); }
input:focus-visible, select:focus-visible, textarea:focus-visible { outline: 2px solid var(--focus-ring); border-color: var(--accent); }
input[type="checkbox"] { accent-color: var(--accent); }
textarea { resize: vertical; line-height: 1.65; }
.prompt-label { font-size: 13px; }
.prompt-input { font-family: var(--font-mono); font-size: 12.5px; line-height: 1.7; }

/* 启用开关：保留原生 checkbox 语义，绘制为开关 */
.settings-editor .check { flex-direction: row; align-items: center; gap: 10px; color: var(--text-primary); cursor: pointer; }
.check input[type="checkbox"] { appearance: none; -webkit-appearance: none; position: relative; width: 38px; height: 22px; margin: 0; flex-shrink: 0; border: 1px solid var(--accent-border); border-radius: 999px; background: var(--bg-hover); cursor: pointer; transition: background .2s var(--ease-out), border-color .2s var(--ease-out); }
.check input[type="checkbox"]::after { content: ''; position: absolute; top: 2px; left: 2px; width: 16px; height: 16px; border-radius: 50%; background: #fff; box-shadow: var(--shadow-xs); transition: transform .22s var(--ease-spring); }
.check input[type="checkbox"]:checked { background: var(--accent); border-color: var(--accent); }
.check input[type="checkbox"]:checked::after { transform: translateX(16px); }
.check input[type="checkbox"]:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }

/* ---------- 工具列表：随编辑区整列滚动，不再套内层滚动条 ---------- */
.tool-list { display: flex; flex-direction: column; border: 1px solid var(--border-soft); border-radius: 12px; background: var(--bg-panel); }
.tools-empty { margin: 0; padding: 20px; color: var(--muted); font-size: 12px; text-align: center; }
.settings-editor .tool-option { flex-direction: row; align-items: flex-start; gap: 10px; padding: 12px 14px; border-bottom: 1px solid var(--border-soft); font-weight: 400; color: var(--text-primary); cursor: pointer; transition: background .15s var(--ease-out); }
.tool-option:last-child { border-bottom: 0; }
.tool-option:hover { background: var(--bg-subtle); }
.tool-option:has(> input:checked) { background: var(--accent-soft); }
.tool-option > input { margin-top: 3px; flex-shrink: 0; }
.tool-option > span { display: flex; flex-direction: column; gap: 4px; min-width: 0; overflow-wrap: anywhere; }
.tool-description { color: var(--text-secondary); font-size: 12px; line-height: 1.6; display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
.unavailable-tool { color: var(--danger); }

/* ---------- 密钥与状态横幅 ---------- */
.key-controls { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 9px 12px; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); background: var(--bg-subtle); color: var(--muted); font-size: 12px; }
.settings-error { display: flex; flex-shrink: 0; align-items: center; flex-wrap: wrap; gap: 10px; margin: 14px 0 0; padding: 10px 14px; border-radius: var(--radius-sm); font-size: 13px; line-height: 1.6; overflow-wrap: anywhere; }
.settings-error::before { content: ''; width: 7px; height: 7px; flex-shrink: 0; border-radius: 50%; background: currentColor; }
.settings-error { color: var(--error-text); background: rgba(220, 38, 38, .06); border: 1px solid rgba(220, 38, 38, .18); }

@media (max-width: 640px) {
  .agent-settings-dialog { width: calc(100vw - 16px); height: calc(100dvh - 20px); border-radius: 16px; }
  .header-icon { width: 36px; height: 36px; border-radius: 10px; }
  .settings-header p { display: none; }
  .settings-tabs { width: 100%; overflow-x: auto; }
  .tab-button { flex: 1 0 auto; min-height: 40px; }
  .settings-layout { grid-template-columns: minmax(0, 1fr); grid-template-rows: auto minmax(0, 1fr); gap: 16px; }
  .field-pair { grid-template-columns: minmax(0, 1fr); }
  .settings-sidebar { flex-direction: row; overflow-x: auto; overflow-y: hidden; }
  .settings-sidebar > button { flex-shrink: 0; width: 160px; }
  .settings-editor input.tool-filter { flex: 1; width: auto; min-width: 120px; font-size: 16px; }
  .button.small { min-height: 40px; }
  .delete-entry { min-height: 40px; }
}
</style>
