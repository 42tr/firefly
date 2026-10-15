<script setup>
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import UiIcon from './UiIcon.vue'
import { portableSettings, useAgentSettings } from '../composables/useAgentSettings'

const props = defineProps({
  active: { type: Boolean, default: true },
  showTrigger: { type: Boolean, default: true },
})
const emit = defineEmits(['closed'])
const { draft, tools, loading, saving, refreshing, error, notice, dirty, load, save, refreshTools, importConfig } = useAgentSettings()
const dialog = ref(null), opened = ref(false), tab = ref('firefly'), selectedAgent = ref(''), selectedProfile = ref(''), importInput = ref(null)
const busy = computed(() => loading.value || saving.value)
const agent = computed(() => !draft.value ? null : tab.value === 'firefly' ? draft.value.firefly : draft.value.subagents.find(item => item.id === selectedAgent.value))
const profile = computed(() => draft.value?.profiles.find(item => item.id === selectedProfile.value))
const missingTools = computed(() => agent.value?.tools.filter(name => !tools.value.some(tool => tool.name === name)) || [])
const newId = prefix => prefix + '-' + Date.now().toString(36) + Math.random().toString(36).slice(2, 6)
const status = child => {
  if (!child.enabled) return '已停用'
  const modelId = child.model_profile || draft.value?.firefly.model_profile
  if (!draft.value?.profiles.find(item => item.id === modelId)?.model?.trim()) return '待配置模型'
  if (child.tools.some(name => !tools.value.some(tool => tool.name === name))) return '工具不可用'
  return '已启用'
}
const show = async () => {
  opened.value = true
  await nextTick(); dialog.value?.showModal()
  if (await load()) {
    selectedAgent.value = draft.value.subagents[0]?.id || ''
    selectedProfile.value = draft.value.profiles[0]?.id || ''
  }
}
defineExpose({ show })
const close = () => {
  if (saving.value) return
  dialog.value?.close(); opened.value = false; emit('closed')
}
const addAgent = () => {
  const id = newId('agent')
  draft.value.subagents.push({ id, name: '新子 agent', description: '', instruction: '', model_profile: null,
    tools: [], enabled: false, timeout_secs: 1800 })
  selectedAgent.value = id
}
const removeAgent = () => {
  draft.value.subagents = draft.value.subagents.filter(item => item !== agent.value)
  selectedAgent.value = draft.value.subagents[0]?.id || ''
}
const addProfile = () => {
  const id = newId('model')
  draft.value.profiles.push({ id, name: '新模型配置', base_url: 'https://api.openai.com/v1', model: '',
    api_key: null, key_configured: false, context_tokens: null })
  selectedProfile.value = id
}
const removeProfile = () => {
  const id = selectedProfile.value
  if (draft.value.firefly.model_profile === id || draft.value.subagents.some(item => item.model_profile === id)) {
    error.value = '此模型配置仍被 agent 使用，请先更换对应 agent 的模型。'; return
  }
  draft.value.profiles = draft.value.profiles.filter(item => item.id !== id)
  selectedProfile.value = draft.value.profiles[0]?.id || ''
}
const exportConfig = () => {
  const url = URL.createObjectURL(new Blob([JSON.stringify(portableSettings(draft.value), null, 2)], { type: 'application/json' }))
  const link = document.createElement('a')
  link.href = url; link.download = 'firefly-agents.json'; link.click()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}
const importFile = async event => {
  const file = event.target.files?.[0]
  try {
    if (!file) return
    if (file.size > 1024 * 1024) throw new Error('配置文件不能超过 1 MiB')
    importConfig(JSON.parse(await file.text()))
    selectedAgent.value = draft.value.subagents[0]?.id || ''
    selectedProfile.value = draft.value.profiles[0]?.id || ''
  } catch (failure) { error.value = failure.message }
  finally { event.target.value = '' }
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
        <div><h2 id="agent-settings-title">Agent 配置</h2><p>配置主助手，按需创建子 agent 并分配工具。</p></div>
        <button class="button ghost small" type="button" :disabled="saving" @click="close">关闭</button>
      </header>
      <div class="settings-content">
        <p v-if="loading" role="status">正在读取配置…</p>
        <template v-if="draft">
          <nav class="settings-tabs" aria-label="配置分类">
            <button v-for="entry in [['firefly', 'Firefly 主 agent'], ['children', '子 agent'], ['models', '模型配置']]" :key="entry[0]" class="button ghost small" type="button" :aria-pressed="tab === entry[0]" :disabled="busy" @click="tab = entry[0]">{{ entry[1] }}<span v-if="entry[0] === 'children'" class="count">{{ draft.subagents.length }}</span></button>
          </nav>
          <form id="agent-settings-form" @submit.prevent="save">
            <fieldset :disabled="busy" class="settings-fields">
              <div class="settings-layout" :class="{ 'root-layout': tab === 'firefly' }">
                <aside v-if="tab === 'children'" class="settings-sidebar" aria-label="子 agent 列表">
                  <button class="button ghost small add-entry" type="button" :disabled="draft.subagents.length >= 50" @click="addAgent">＋ 新建子 agent</button>
                  <button v-for="child in draft.subagents" :key="child.id" class="entry-card" type="button" :aria-pressed="selectedAgent === child.id" @click="selectedAgent = child.id"><strong>{{ child.name }}</strong><span>{{ status(child) }}</span></button>
                  <p v-if="!draft.subagents.length" class="empty-hint">尚未创建子 agent。主助手可以独立工作。</p>
                </aside>
                <aside v-if="tab === 'models'" class="settings-sidebar" aria-label="模型配置列表">
                  <button class="button ghost small add-entry" type="button" :disabled="draft.profiles.length >= 32" @click="addProfile">＋ 新建模型配置</button>
                  <button v-for="item in draft.profiles" :key="item.id" class="entry-card" type="button" :aria-pressed="selectedProfile === item.id" @click="selectedProfile = item.id"><strong>{{ item.name }}</strong><span>{{ item.model || '待配置模型' }}</span></button>
                </aside>
                <section v-if="agent && tab !== 'models'" class="settings-editor" :aria-label="tab === 'firefly' ? 'Firefly 主 agent 设置' : '子 agent 设置'">
                  <div class="editor-heading"><h3>{{ tab === 'firefly' ? 'Firefly 主 agent' : agent.name }}</h3><button v-if="tab === 'children'" class="button ghost small delete-entry" type="button" @click="removeAgent">删除子 agent</button></div>
                  <label v-if="tab === 'children'" class="check"><input v-model="agent.enabled" type="checkbox" />启用子 agent</label>
                  <div class="field-pair">
                    <label>名称<input v-model="agent.name" maxlength="100" required /></label>
                    <label v-if="tab === 'children'">标识<input :value="agent.id" aria-label="标识" maxlength="40" pattern="[a-z0-9_-]+" required @input="renameAgentId" /><small>小写字母、数字、下划线或连字符。</small></label>
                  </div>
                  <label v-if="tab === 'children'">职责说明<textarea v-model="agent.description" rows="2" maxlength="4096" required placeholder="主助手根据这段说明选择何时委派任务。"></textarea></label>
                  <div class="field-pair">
                    <label>使用模型<select v-model="agent.model_profile"><option v-if="tab === 'children'" :value="null">继承 Firefly 的模型</option><option v-for="item in draft.profiles" :key="item.id" :value="item.id">{{ item.name }}{{ item.model ? ' · ' + item.model : ' · 待配置' }}</option></select></label>
                    <label>执行期限（秒）<input v-model.number="agent.timeout_secs" type="number" min="1" max="604800" required /></label>
                  </div>
                  <label>系统提示词<textarea v-model="agent.instruction" class="prompt-input" rows="8" maxlength="131072" placeholder="填写角色、工作流程、约束和返回要求。"></textarea></label>
                  <div class="tools-heading"><h4>可用工具</h4><button class="button ghost small" type="button" :disabled="refreshing" @click="refreshTools">{{ refreshing ? '读取中…' : '刷新工具' }}</button></div>
                  <p class="field-hint">勾选要分配给此 agent 的工具。扩展工具需先在“扩展模块”中启用。</p>
                  <div class="tool-list">
                    <label v-for="tool in tools" :key="tool.name" class="tool-option"><input v-model="agent.tools" :value="tool.name" type="checkbox" /><span><strong>{{ tool.name }}</strong><small>{{ tool.source }} · {{ tool.read_only ? '只读' : '可执行操作' }}</small><span class="tool-description">{{ tool.description }}</span></span></label>
                    <label v-for="name in missingTools" :key="name" class="tool-option unavailable-tool"><input v-model="agent.tools" :value="name" type="checkbox" /><span><strong>{{ name }}</strong><small>工具不可用，请启用对应模块或取消选择。</small></span></label>
                  </div>
                </section>
                <section v-else-if="tab === 'models' && profile" class="settings-editor" aria-label="模型配置设置">
                  <div class="editor-heading"><h3>{{ profile.name }}</h3><button class="button ghost small delete-entry" type="button" @click="removeProfile">删除模型配置</button></div>
                  <label>配置名称<input v-model="profile.name" maxlength="100" required /></label>
                  <label>服务地址<input v-model.trim="profile.base_url" aria-label="服务地址" type="url" maxlength="2048" required placeholder="https://api.openai.com/v1" /><small>使用支持 Responses API 的模型服务。</small></label>
                  <label>模型名称<input v-model.trim="profile.model" maxlength="256" placeholder="填写服务支持的模型名称" /></label>
                  <label>API Key<input v-model="profile.api_key" aria-label="API Key" type="password" maxlength="16384" autocomplete="new-password" :placeholder="profile.key_configured ? '已保存，同一服务下留空保留' : '填写密钥；无认证服务可留空'" @input="profile.clear_key = false" /><small>更换协议、域名或端口后，留空保存会清除原密钥。</small></label>
                  <div class="key-controls"><span>{{ profile.clear_key ? '保存后清除密钥' : profile.key_configured ? '已有密钥' : '尚未保存密钥' }}</span><button class="button ghost small" type="button" @click="profile.api_key = null; profile.clear_key = !profile.clear_key">{{ profile.clear_key ? '保留原密钥' : '清除密钥' }}</button></div>
                  <label>输入预算<input v-model.number="profile.context_tokens" aria-label="输入预算" type="number" min="1024" max="100000000" placeholder="留空自动查询模型上下文窗口" /><small>查询失败时可填写显式预算作为兜底。</small></label>
                </section>
              </div>
            </fieldset>
          </form>
        </template>
        <p v-if="error" class="settings-error" role="alert">{{ error }}<button v-if="!draft" class="button ghost small" type="button" :disabled="loading" @click="show">重新读取</button></p>
        <p v-if="notice" class="settings-notice" role="status">{{ notice }}</p>
      </div>
      <footer class="settings-footer">
        <div class="transfer-actions"><button class="button ghost small" type="button" :disabled="!draft || busy" @click="importInput.click()">导入配置</button><button class="button ghost small" type="button" :disabled="!draft || busy" @click="exportConfig">导出配置</button><input ref="importInput" type="file" accept="application/json,.json" hidden aria-label="导入 Agent 配置文件" @change="importFile" /><small>导出文件不包含 API Key。</small></div>
        <div class="save-actions"><span v-if="dirty">有未保存的修改</span><button class="button primary small" type="submit" form="agent-settings-form" :disabled="!draft || busy">{{ saving ? '保存中…' : '保存配置' }}</button></div>
      </footer>
    </dialog>
  </Teleport>
</template>

<style scoped>
.agent-settings-dialog { width: min(960px, calc(100vw - 32px)); max-width: none; max-height: calc(100dvh - 40px); padding: 0; overflow: hidden; border: 1px solid var(--border-soft); border-radius: 18px; color: var(--text-primary); background: var(--bg-panel); box-shadow: var(--shadow-dialog); }
.agent-settings-dialog[open] { display: flex; flex-direction: column; }
.agent-settings-dialog::backdrop { background: rgba(17, 25, 40, .4); }
.settings-header, .settings-footer { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-shrink: 0; padding: 18px 24px; }
.settings-header { border-bottom: 1px solid var(--border-soft); }
.settings-header h2 { margin: 0; font-size: 18px; }.settings-header p { margin: 6px 0 0; color: var(--muted); font-size: 13px; }
.settings-content { padding: 20px 24px; overflow-y: auto; min-height: 0; }
.settings-tabs { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 20px; }
.settings-tabs [aria-pressed="true"] { background: var(--bg-hover); color: var(--accent); }
.count { padding: 1px 6px; border-radius: 8px; background: var(--bg-subtle); }
.settings-fields { min-width: 0; border: 0; padding: 0; margin: 0; }
.settings-layout { display: grid; grid-template-columns: 190px minmax(0, 1fr); gap: 24px; }.root-layout { grid-template-columns: minmax(0, 1fr); }
.settings-sidebar { display: flex; flex-direction: column; align-items: stretch; gap: 8px; min-width: 0; }.add-entry { text-align: left; }
.entry-card { display: flex; flex-direction: column; align-items: flex-start; gap: 5px; width: 100%; padding: 12px; border: 1px solid var(--border-soft); border-radius: 10px; background: transparent; color: var(--text-primary); font: inherit; cursor: pointer; overflow-wrap: anywhere; text-align: left; }
.entry-card[aria-pressed="true"] { border-color: var(--accent); background: var(--bg-hover); }.entry-card span { color: var(--muted); font-size: 12px; }
.empty-hint, .field-hint { font-size: 12px; color: var(--muted); line-height: 1.7; }
.settings-editor { display: flex; flex-direction: column; gap: 16px; min-width: 0; }.editor-heading, .tools-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
h3, h4 { margin: 0; font-size: 15px; }.delete-entry { color: var(--danger, #d54f56); }
.settings-editor label { display: flex; flex-direction: column; gap: 7px; font-size: 13px; }.settings-editor small { color: var(--muted); line-height: 1.6; font-size: 12px; }
.field-pair { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; }
input:not([type="checkbox"]), select, textarea { width: 100%; box-sizing: border-box; min-width: 0; padding: 9px 11px; border: 1px solid var(--border-soft); border-radius: 8px; color: var(--text-primary); background: var(--bg-subtle); font: inherit; font-size: 13px; }
textarea { resize: vertical; line-height: 1.65; }.prompt-input { font-family: ui-monospace, monospace; }
input:focus-visible, select:focus-visible, textarea:focus-visible, .entry-card:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
.settings-editor .check { flex-direction: row; align-items: center; }.key-controls { display: flex; align-items: center; justify-content: space-between; color: var(--muted); font-size: 12px; margin-top: -8px; }
.tool-list { display: flex; flex-direction: column; gap: 1px; max-height: 320px; overflow-y: auto; border: 1px solid var(--border-soft); border-radius: 10px; }
.settings-editor .tool-option { flex-direction: row; align-items: flex-start; gap: 10px; padding: 12px; border-bottom: 1px solid var(--border-soft); }.tool-option:last-child { border: 0; }.tool-option > input { margin-top: 3px; flex-shrink: 0; }.tool-option > span { display: flex; flex-direction: column; gap: 4px; min-width: 0; overflow-wrap: anywhere; }
.tool-description { color: var(--text-secondary); font-size: 12px; line-height: 1.6; display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }.unavailable-tool { color: var(--danger, #d54f56); }
.settings-error { color: var(--danger, #d54f56); font-size: 13px; overflow-wrap: anywhere; }.settings-notice { color: var(--text-secondary); font-size: 13px; }
.settings-footer { border-top: 1px solid var(--border-soft); }.transfer-actions, .save-actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }.settings-footer small, .save-actions span { font-size: 12px; color: var(--muted); }
@media (max-width: 640px) {
  .agent-settings-dialog { width: calc(100vw - 16px); max-height: calc(100dvh - 20px); border-radius: 14px; }
  .settings-header, .settings-content, .settings-footer { padding: 16px; }.settings-layout { grid-template-columns: minmax(0, 1fr); gap: 18px; }.field-pair { grid-template-columns: minmax(0, 1fr); }
  .settings-sidebar { flex-direction: row; overflow-x: auto; }.settings-sidebar > button { flex-shrink: 0; width: 150px; }.settings-footer { flex-direction: column; align-items: stretch; }.save-actions { justify-content: flex-end; }.settings-header p { max-width: 220px; line-height: 1.6; }
  .button.small { min-height: 40px; }.tool-list { max-height: 260px; }
}
</style>
