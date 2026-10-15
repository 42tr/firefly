<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref, useId, watch } from 'vue'
import UiIcon from './UiIcon.vue'
import { apiFetch } from '@/utils/http'
import { useCodexModels } from '@/composables/useCodexModels'
import { useClaudeModels } from '@/composables/useClaudeModels'
import ClaudeSettingsFields from './ClaudeSettingsFields.vue'

const props = defineProps({
  open: { type: Boolean, default: false },
  defaultAgent: { type: String, default: 'firefly' },
  preset: { type: Object, default: null },
  creating: Boolean,
})

const emit = defineEmits(['change', 'busy'])
const pane = ref(null)
const controls = ref(null)
const agentTrigger = ref(null)
const agentMenu = ref(null)
const agentMenuId = useId()
const workspacePanelId = useId()
const settingsPanelId = useId()
const providerSelectId = useId()
const modelSelectId = useId()
const reasoningSelectId = useId()
const parallelDescriptionId = useId()
const confirmedPath = ref('')

const agent = ref(['firefly', 'codex', 'claude'].includes(props.defaultAgent) ? props.defaultAgent : 'firefly')
const agentOptions = [
  { id: 'firefly', name: '流萤', icon: 'chat' },
  { id: 'codex', name: 'Codex', icon: 'terminal' },
  { id: 'claude', name: 'Claude', icon: 'tools' },
]
const {
  providers, modelsLoading, modelsError, defaultProvider, defaultModel,
  provider, model, reasoningLevel, loadProviders, modelOptions, providerModelsError,
  reasoningOptions, changeProvider, cancelModelLoad,
} = useCodexModels(agent)
const {
  model: claudeModel, modelOptions: claudeModelOptions,
  provider: claudeProvider, providers: claudeProviders,
  reasoningLevel: claudeReasoningLevel, reasoningOptions: claudeReasoningOptions,
  changeProvider: changeClaudeProvider,
  modelsLoading: claudeModelsLoading, modelsError: claudeModelsError,
  loadModels: loadClaudeModels, cancelModelLoad: cancelClaudeModelLoad,
} = useClaudeModels()
const path = ref('')
const parallel = ref(false)
const entries = ref([])
const directoryInput = ref(null)
const newDirectoryTrigger = ref(null)
const workspacePanel = ref(null)
let previousFocus = null
const breadcrumbs = computed(() => {
  const parts = path.value.split('/').filter(Boolean)
  return parts.map((name, index) => ({ name, path: parts.slice(0, index + 1).join('/') }))
})
const isLoading = ref(false)
const isCreatingDirectory = ref(false)
const showNewDirectory = ref(false)
const newDirectoryName = ref('')
const error = ref('')
let workspaceRequest = 0

const loadWorkspace = async () => {
  if (!['codex', 'claude'].includes(agent.value)) return
  const request = ++workspaceRequest
  isLoading.value = true
  error.value = ''
  try {
    const params = new URLSearchParams()
    if (path.value) params.set('path', path.value)
    const query = params.toString()
    const resp = await apiFetch(`/api/assistant/workspace${query ? `?${query}` : ''}`, { credentials: 'include' })
    if (!resp.ok) throw new Error((await resp.text()) || '读取 workspace 失败')
    const data = await resp.json()
    if (request !== workspaceRequest) return
    entries.value = data.entries || []
    path.value = data.path || ''
  } catch (err) {
    if (request !== workspaceRequest) return
    error.value = err.message || '读取 workspace 失败'
    entries.value = []
  } finally {
    if (request === workspaceRequest) isLoading.value = false
  }
}

const enterDirectory = (entry) => {
  showNewDirectory.value = false
  newDirectoryName.value = ''
  path.value = entry.path
  loadWorkspace()
}

const goParent = () => {
  if (!path.value) return
  showNewDirectory.value = false
  newDirectoryName.value = ''
  const parts = path.value.split('/').filter(Boolean)
  parts.pop()
  path.value = parts.join('/')
  loadWorkspace()
}

const openNewDirectory = async () => {
  error.value = ''
  newDirectoryName.value = ''
  showNewDirectory.value = true
  await nextTick()
  directoryInput.value?.focus()
}

const closeNewDirectory = () => {
  if (isCreatingDirectory.value) return
  showNewDirectory.value = false
  newDirectoryName.value = ''
  nextTick(() => newDirectoryTrigger.value?.focus())
}

const createDirectory = async () => {
  const name = newDirectoryName.value.trim()
  if (!name) {
    error.value = '请输入目录名称'
    return
  }
  isCreatingDirectory.value = true
  error.value = ''
  try {
    const resp = await apiFetch('/api/assistant/workspace', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ path: path.value || null, name }),
    })
    if (!resp.ok) throw new Error((await resp.text()) || '创建目录失败')
    const entry = await resp.json()
    showNewDirectory.value = false
    newDirectoryName.value = ''
    path.value = entry.path
    await loadWorkspace()
  } catch (err) {
    error.value = err.message || '创建目录失败'
  } finally {
    isCreatingDirectory.value = false
  }
}

const resetPicker = () => {
  workspaceRequest += 1
  cancelModelLoad()
  cancelClaudeModelLoad()
  isLoading.value = false
  provider.value = ''
  model.value = ''
  claudeModel.value = ''
  claudeProvider.value = ''
  claudeReasoningLevel.value = ''
  reasoningLevel.value = ''
  path.value = ''
  confirmedPath.value = ''
  parallel.value = false
  entries.value = []
  showNewDirectory.value = false
  newDirectoryName.value = ''
  error.value = ''
}

const applyWorkspace = (item) => {
  if (props.creating || !item) return
  resetPicker()
  agent.value = item.agent
  parallel.value = item.workspace_mode === 'parallel'
  path.value = item.cwd || ''
  confirmedPath.value = path.value
  provider.value = item.codex_provider === 'openai' ? '' : item.codex_provider || item.provider || ''
  model.value = item.codex_model || item.model || ''
  claudeModel.value = item.claude_model || item.model || ''
  claudeProvider.value = item.claude_provider || item.provider || ''
  claudeReasoningLevel.value = item.claude_reasoning_level || item.reasoning_level || ''
  reasoningLevel.value = item.codex_reasoning_level || item.reasoning_level || ''
  if (agent.value === 'codex') loadProviders(false)
  if (agent.value === 'claude') loadClaudeModels()
  loadWorkspace()
}
const changeAgent = (value) => {
  if (props.creating || agent.value === value) return
  agent.value = value
  resetPicker()
  if (value === 'codex') loadProviders()
  if (value === 'claude') loadClaudeModels()
  loadWorkspace()
  pane.value = null
  nextTick(() => agentTrigger.value?.focus())
}
const cancelWorkspaceListing = () => {
  workspaceRequest += 1
  isLoading.value = false
  error.value = ''
}
const close = () => {
  if (props.creating) return
  if (pane.value === 'workspace') cancelWorkspaceListing()
  pane.value = null
  nextTick(() => previousFocus?.focus())
}
const settingsKeydown = event => {
  if (event.key !== 'Escape' || event.isComposing) return
  event.preventDefault()
  event.stopPropagation()
  close()
}
const selection = computed(() => ({
  agent: agent.value,
  workspace_mode: agent.value !== 'firefly' && parallel.value ? 'parallel' : 'shared',
  cwd: agent.value === 'firefly' ? null : confirmedPath.value,
  provider: agent.value === 'codex' ? provider.value : agent.value === 'claude' ? claudeProvider.value : null,
  model: agent.value === 'codex' ? model.value : agent.value === 'claude' ? claudeModel.value : null,
  reasoning_level: agent.value === 'codex' ? reasoningLevel.value : agent.value === 'claude' ? claudeReasoningLevel.value : null,
}))
const directoryLabel = computed(() => confirmedPath.value.split('/').filter(Boolean).at(-1) || 'workspace')
const directoryTitle = computed(() => `workspace/${confirmedPath.value}`)
const busy = computed(() => isLoading.value || isCreatingDirectory.value || !!error.value || (agent.value === 'codex' && modelsLoading.value) || (agent.value === 'claude' && claudeModelsLoading.value))
watch(selection, value => emit('change', value), { flush: 'post' })
watch(busy, value => emit('busy', value), { flush: 'sync' })
watch(() => props.open, open => {
  if (!open) { pane.value = null; return }
  if (props.preset) applyWorkspace(props.preset)
  else {
    agent.value = agentOptions.some(option => option.id === props.defaultAgent) ? props.defaultAgent : 'firefly'
    resetPicker()
    if (agent.value === 'codex') loadProviders()
    if (agent.value === 'claude') loadClaudeModels()
    loadWorkspace()
  }
  emit('change', selection.value)
}, { immediate: true })
const openPane = async (value, event) => {
  if (props.creating) return
  previousFocus = event?.currentTarget || agentTrigger.value
  if (pane.value === value) { close(); return }
  if (pane.value === 'workspace') cancelWorkspaceListing()
  pane.value = value
  if (value === 'workspace') {
    path.value = confirmedPath.value
    showNewDirectory.value = false
    loadWorkspace()
  }
  await nextTick()
  if (value === 'agent') agentMenu.value?.querySelector('[aria-checked="true"]')?.focus()
  else if (value === 'workspace') workspacePanel.value?.focus()
}
const confirmPane = () => {
  if (busy.value || props.creating) return
  if (pane.value === 'workspace') {
    confirmedPath.value = path.value
  }
  close()
}
const outside = event => {
  if (pane.value === 'agent' && !controls.value?.contains(event.target)) close()
}
const navigateAgents = event => {
  if (event.key === 'Escape') { event.preventDefault(); close(); return }
  if (event.key === 'Tab') { close(); return }
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
  event.preventDefault()
  const buttons = [...agentMenu.value.querySelectorAll('[role="menuitemradio"]')]
  const index = buttons.indexOf(document.activeElement)
  const target = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1
    : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length
  buttons[target]?.focus()
}
onMounted(() => document.addEventListener('pointerdown', outside))
onBeforeUnmount(() => {
  workspaceRequest += 1
  cancelModelLoad()
  cancelClaudeModelLoad()
  document.removeEventListener('pointerdown', outside)
})
</script>

<template>
  <fieldset v-if="open && pane === 'settings' && agent !== 'firefly'" :id="settingsPanelId" class="inline-settings" :disabled="creating" :aria-label="agentOptions.find(option => option.id === agent)?.name + ' 配置'" @keydown="settingsKeydown">
    <div v-if="agent === 'codex'" class="model-settings">
      <div class="provider-picker">
        <label class="field-label" :for="providerSelectId">API 平台</label>
        <select :id="providerSelectId" v-model="provider" class="provider-select" :disabled="modelsLoading" @change="changeProvider">
          <option value="">openai</option>
          <option v-for="item in providers" :key="item.name" :value="item.name">{{ item.name }}{{ item.name === defaultProvider ? '（默认）' : '' }}</option>
        </select>
      </div>
      <div class="provider-picker">
        <label class="field-label" :for="modelSelectId">模型</label>
        <select v-if="modelsLoading || modelOptions.length" :id="modelSelectId" v-model="model" class="provider-select" :disabled="modelsLoading">
          <option v-if="modelsLoading" value="" disabled>加载模型中…</option>
          <option v-if="model && !modelOptions.includes(model)" :value="model">{{ model }}</option>
          <option v-for="item in modelOptions" :key="item" :value="item">{{ item }}</option>
        </select>
        <input v-else :id="modelSelectId" v-model.trim="model" class="provider-select" type="text" placeholder="模型名称" />
      </div>
      <div v-if="reasoningOptions.length" class="provider-picker">
        <label class="field-label" :for="reasoningSelectId">推理级别</label>
        <select :id="reasoningSelectId" v-model="reasoningLevel" class="provider-select" :disabled="modelsLoading">
          <option v-if="modelsLoading" value="" disabled>加载中…</option>
          <option v-for="level in reasoningOptions" :key="level" :value="level">{{ level }}</option>
        </select>
      </div>
      <p v-if="modelsError || providerModelsError" class="models-note" role="status">{{ modelsError || providerModelsError }}</p>
    </div>

    <div v-if="agent === 'claude'" class="claude-model-settings">
      <ClaudeSettingsFields v-model="claudeModel" v-model:provider="claudeProvider" v-model:reasoning-level="claudeReasoningLevel" :providers="claudeProviders" :reasoning-levels="claudeReasoningOptions" @change-provider="changeClaudeProvider" :models="claudeModelOptions" :disabled="creating || claudeModelsLoading" />
      <p v-if="claudeModelsLoading" class="models-note" role="status">正在读取模型选项…</p>
      <p v-else-if="claudeModelsError" class="models-note" role="status">{{ claudeModelsError }} <button class="button ghost small" type="button" @click="loadClaudeModels">重试</button></p>
    </div>

    <label class="parallel-option" :class="{ selected: parallel }">
      <span class="parallel-copy"><strong>并行模式</strong><small :id="parallelDescriptionId">在独立副本中工作</small></span>
      <input v-model="parallel" class="parallel-switch" type="checkbox" role="switch" aria-label="并行模式" :aria-describedby="parallelDescriptionId" />
    </label>
  </fieldset>
  <fieldset v-if="open && pane === 'workspace' && agent !== 'firefly'" :id="workspacePanelId" ref="workspacePanel" class="inline-workspace" :disabled="creating" aria-label="选择工作目录" tabindex="-1" @keydown="settingsKeydown">
    <div class="workspace-heading">
      <span class="section-label">工作目录</span>
      <div class="workspace-header-actions">
        <button v-if="path" class="button quiet small" type="button" :disabled="isLoading || isCreatingDirectory" @click="goParent">上一级</button>
        <button ref="newDirectoryTrigger" class="button quiet small" type="button" :disabled="isLoading || isCreatingDirectory" @click="openNewDirectory">新建目录</button>
      </div>
    </div>
    <div class="workspace-picker" :aria-busy="isLoading">
      <nav class="workspace-path" aria-label="当前工作目录">
        <UiIcon name="folder" />
        <button type="button" :disabled="isLoading || isCreatingDirectory" :aria-current="!path ? 'location' : undefined" @click="enterDirectory({ path: '' })">workspace</button>
        <template v-for="(crumb, index) in breadcrumbs" :key="crumb.path">
          <span class="path-divider" aria-hidden="true">/</span>
          <button type="button" :disabled="isLoading || isCreatingDirectory" :aria-current="index === breadcrumbs.length - 1 ? 'location' : undefined" @click="enterDirectory(crumb)">{{ crumb.name }}</button>
        </template>
        <span v-if="!path" class="path-divider" aria-hidden="true">/</span>
      </nav>
      <form v-if="showNewDirectory" class="new-directory-form" @submit.prevent="createDirectory">
        <input ref="directoryInput" v-model="newDirectoryName" type="text" maxlength="120" autocomplete="off" placeholder="输入新目录名称" aria-label="新目录名称" :disabled="isCreatingDirectory" @keydown.esc.stop="closeNewDirectory" />
        <button class="button quiet small" type="button" :disabled="isCreatingDirectory" @click="closeNewDirectory">取消</button>
        <button class="button primary small" type="submit" :disabled="isCreatingDirectory || !newDirectoryName.trim()">{{ isCreatingDirectory ? '创建中…' : '创建' }}</button>
      </form>
      <div class="directory-content">
        <div v-if="error" class="directory-state directory-error" role="alert">{{ error }}<button class="button ghost small" type="button" :disabled="isLoading" @click="loadWorkspace">重试</button></div>
        <div v-else-if="isLoading" class="directory-state" role="status">正在读取目录…</div>
        <div v-else-if="!entries.length" class="directory-state"><UiIcon name="folder" /><span>此目录下还没有文件夹</span><small>可以直接开始工作，或新建一个目录。</small></div>
        <div v-else class="workspace-list">
          <button v-for="entry in entries" :key="entry.path" class="workspace-entry" type="button" :title="entry.name" :aria-label="`进入目录 ${entry.name}`" :disabled="isCreatingDirectory" @click="enterDirectory(entry)">
            <UiIcon name="folder" /><span class="entry-name">{{ entry.name }}</span><UiIcon name="down" class="entry-arrow" />
          </button>
        </div>
      </div>
    </div>
    <div class="workspace-actions">
      <span class="workspace-hint">在当前目录中开始工作</span>
      <button class="button quiet small" type="button" @click="close">取消</button>
      <button class="button primary small" type="button" :disabled="busy" @click="confirmPane">使用此目录</button>
    </div>
  </fieldset>
  <div v-if="open" ref="controls" class="new-conversation-controls">
    <div class="agent-picker">
      <button ref="agentTrigger" class="creation-control agent-control" type="button" aria-label="选择 Agent" aria-haspopup="menu" :aria-expanded="pane === 'agent'" :aria-controls="agentMenuId" :disabled="creating" @click="openPane('agent', $event)" @keydown.down.prevent="openPane('agent', $event)">
        <UiIcon class="agent-control-icon" :name="agentOptions.find(option => option.id === agent)?.icon" />
        <span :key="agent" class="agent-control-label">{{ agentOptions.find(option => option.id === agent)?.name }}</span>
        <UiIcon class="agent-control-chevron" name="chevron-down" />
      </button>
      <div v-if="pane === 'agent'" :id="agentMenuId" ref="agentMenu" class="agent-selection-menu" role="menu" aria-label="选择 Agent" @keydown="navigateAgents">
        <button v-for="option in agentOptions" :key="option.id" type="button" role="menuitemradio" :aria-checked="agent === option.id" :disabled="creating" @click="changeAgent(option.id); close()">
          <UiIcon class="agent-option-icon" :name="option.icon" />
          <span class="agent-option-name">{{ option.name }}</span>
          <UiIcon v-if="agent === option.id" class="agent-option-check" name="check-square" />
        </button>
      </div>
    </div>
    <button v-if="agent !== 'firefly'" class="creation-control directory-control" :class="{ expanded: pane === 'workspace' }" type="button" aria-label="选择工作目录" :title="directoryTitle" :aria-expanded="pane === 'workspace'" :aria-controls="workspacePanelId" :disabled="creating" @click="openPane('workspace', $event)">
      <UiIcon name="folder" /><span>{{ directoryLabel }}</span><UiIcon v-if="pane === 'workspace'" class="collapse-icon" name="chevron-down" />
    </button>
    <button v-if="agent !== 'firefly'" class="creation-control settings-control" :class="{ expanded: pane === 'settings' }" type="button" :aria-label="pane === 'settings' ? '收起 Agent 配置' : '配置 Agent'" :aria-expanded="pane === 'settings'" :aria-controls="settingsPanelId" :disabled="creating" @click="openPane('settings', $event)"><UiIcon name="sliders" /><span>{{ pane === 'settings' ? '收起' : '配置' }}</span><UiIcon v-if="pane === 'settings'" class="collapse-icon" name="chevron-down" /></button>
  </div>
  <p v-if="open && error && pane !== 'workspace'" class="creation-error" role="alert">{{ error }} <button type="button" @click="loadWorkspace">重试</button></p>
</template>

<style scoped>
.new-conversation-controls { position: relative; display: flex; align-items: center; gap: 16px; width: 100%; min-width: 0; padding-bottom: 8px; }
.creation-control { display: inline-flex; align-items: center; gap: 8px; min-height: 44px; padding: 0 6px; border: 0; border-radius: 8px; background: transparent; color: var(--text-secondary); font: inherit; font-size: 14px; cursor: pointer; transition: color 180ms var(--ease-out), transform 180ms var(--ease-out); }
.creation-control:hover:not(:disabled) { color: var(--text-primary); }
.creation-control:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
.creation-control:disabled { opacity: .5; cursor: wait; }
.creation-control .ui-icon { width: 18px; height: 18px; flex-shrink: 0; }
.creation-control .ui-icon:last-child { width: 14px; height: 14px; }
.agent-picker { position: relative; flex-shrink: 0; }
.agent-control { color: var(--text-secondary); }
.agent-control[aria-expanded='true'] { color: var(--text-primary); }
.agent-control:active:not(:disabled) { transform: scale(.97); }
.agent-control-icon { transition: transform 240ms var(--ease-out); }
.agent-control-label { animation: agentLabelIn 180ms var(--ease-out); }
.agent-control .agent-control-chevron { color: var(--muted); transition: transform 240ms var(--ease-out), color 180ms var(--ease-out); }
.agent-control[aria-expanded='true'] .agent-control-chevron { color: var(--text-primary); transform: rotate(180deg); }
@media (hover: hover) and (pointer: fine) {
  .agent-control:hover:not(:disabled) .agent-control-icon { transform: translateY(-1px); }
}
.directory-control { min-width: 0; max-width: 55%; }
.directory-control span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.settings-control { margin-left: auto; flex-shrink: 0; }
.settings-control .ui-icon { width: 18px; height: 18px; }
.creation-control .collapse-icon { width: 14px; height: 14px; transform: rotate(180deg); }
.directory-control.expanded, .settings-control.expanded { color: var(--text-primary); }
.model-settings, .claude-model-settings :deep(.claude-settings-fields) { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 12px; margin-top: -4px; }
.provider-picker, .claude-model-settings :deep(.setting-field), .claude-model-settings :deep(.claude-model-picker) { display: block; min-width: 0; }
.field-label, .claude-model-settings :deep(label) { display: block; margin-bottom: 7px; color: var(--text-secondary); font-size: 12px; }
.provider-select, .claude-model-settings :deep(select), .claude-model-settings :deep(input) { width: 100%; min-width: 0; height: 38px; min-height: 0; padding: 0 9px; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); background: var(--bg-panel); color: var(--text-primary); font: inherit; font-size: 12px; text-overflow: ellipsis; }
.claude-model-settings :deep(label:not(:first-child)) { margin-top: 8px; }
.claude-model-settings :deep(.effort-hint) { grid-column: 1 / -1; }
.inline-settings { display: flex; flex-direction: column; gap: 16px; min-width: 0; margin: 0 0 12px; padding: 4px 6px 0; border: 0; }
.inline-settings .model-settings, .inline-settings .claude-model-settings :deep(.claude-settings-fields) { margin: 0; }
.inline-settings .field-label, .inline-settings .claude-model-settings :deep(label) { font-size: 13px; }
.inline-settings .provider-select, .inline-settings .claude-model-settings :deep(select), .inline-settings .claude-model-settings :deep(input) { font-size: 14px; }
.inline-settings .parallel-option { min-height: 52px; padding: 12px 0 0; border-top: 1px solid var(--border-soft); border-radius: 0; background: transparent; }
.inline-settings .parallel-copy strong { font-size: 14px; }
@media (max-width: 768px) {
  .inline-settings { gap: 0; padding-top: 0; max-height: min(260px, calc(var(--app-viewport-height, 100dvh) * .4)); overflow-y: auto; overscroll-behavior: contain; }
  .inline-settings .model-settings, .inline-settings .claude-model-settings :deep(.claude-settings-fields) { grid-template-columns: 1fr; gap: 0; }
  .inline-settings .provider-picker, .inline-settings .claude-model-settings :deep(.setting-field), .inline-settings .claude-model-settings :deep(.claude-model-picker) { display: grid; grid-template-columns: 80px minmax(0, 1fr); align-items: center; gap: 12px; min-height: 56px; padding: 6px 0; border-bottom: 1px solid var(--border-soft); }
  .inline-settings .field-label, .inline-settings .claude-model-settings :deep(label) { margin: 0; }
  .inline-settings .provider-select, .inline-settings .claude-model-settings :deep(select), .inline-settings .claude-model-settings :deep(input) { height: 44px; font-size: 16px; }
  .inline-settings .models-note { padding: 8px 0; }
  .inline-settings .claude-model-settings :deep(.effort-hint) { padding: 4px 0; }
  .inline-settings .model-settings + .parallel-option, .inline-settings .claude-model-settings + .parallel-option { border-top: 0; }
  .inline-settings .parallel-option { min-height: 60px; padding: 10px 0 0; }
}
.agent-selection-menu { position: absolute; left: 0; bottom: calc(100% + 8px); z-index: 25; width: 216px; max-width: calc(100vw - 32px); max-height: min(320px, calc(var(--app-viewport-height, 100dvh) - 160px)); overflow-y: auto; overscroll-behavior: contain; padding: 6px; border: 1px solid var(--border-soft); border-radius: 16px; background: var(--bg-panel); box-shadow: var(--shadow-float); transform-origin: 22px bottom; animation: agentMenuIn 200ms var(--ease-out); }
.agent-selection-menu button { display: flex; align-items: center; gap: 12px; width: 100%; min-height: 44px; padding: 8px 12px; border: 0; border-radius: 10px; background: transparent; color: var(--text-primary); font: inherit; font-size: 14px; text-align: left; cursor: pointer; transition: background 160ms var(--ease-out); }
.agent-selection-menu button:hover:not(:disabled), .agent-selection-menu button:focus-visible { background: var(--bg-hover); }
.agent-option-icon { width: 18px; height: 18px; color: var(--text-secondary); }
.agent-option-name { flex: 1; min-width: 0; }
.agent-option-check { width: 16px; height: 16px; color: var(--text-secondary); }
@keyframes agentMenuIn { from { opacity: 0; transform: translateY(6px) scale(.97); } to { opacity: 1; transform: translateY(0) scale(1); } }
@keyframes agentLabelIn { from { opacity: 0; transform: translateY(3px); } to { opacity: 1; transform: translateY(0); } }
.creation-error { margin: 0 0 8px; color: var(--danger); font-size: 13px; }
.creation-error button { color: inherit; font: inherit; }
@media (max-width: 768px) { .new-conversation-controls { gap: 8px; } .directory-control { flex: 0 1 auto; max-width: none; } }

.inline-workspace { display: flex; flex-direction: column; gap: 10px; min-width: 0; max-height: min(340px, calc(var(--app-viewport-height, 100dvh) * .4)); margin: 0 0 12px; padding: 4px 6px 0; border: 0; background: transparent; overflow-y: auto; overscroll-behavior: contain; }
.inline-workspace:focus { outline: none; }
.inline-workspace .workspace-heading { flex: 0 0 auto; margin: 0; }
.inline-workspace .workspace-picker { flex: 0 0 auto; }
.workspace-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; padding-top: 10px; border-top: 1px solid var(--border-soft); }
.workspace-actions .button { min-height: 36px; white-space: nowrap; }
.workspace-hint { flex: 1; min-width: 0; color: var(--muted); font-size: 12px; }
.section-label { display: block; color: var(--text-primary); font-size: 13px; font-weight: 600; }
.models-note { grid-column: 1 / -1; margin: 0; color: var(--muted); font-size: 12px; line-height: 1.5; }
.workspace-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 8px; }
.workspace-header-actions { display: flex; align-items: center; gap: 8px; min-width: 0; }
.workspace-header-actions .button { min-height: 28px; padding: 3px 6px; font-size: 12px; white-space: nowrap; }
.workspace-picker { min-width: 0; overflow: hidden; }
.workspace-path { display: flex; align-items: center; flex-wrap: wrap; gap: 5px; min-height: 40px; padding: 8px 3px; border-bottom: 1px solid var(--border-soft); font-family: var(--font-mono); font-size: 12px; }
.workspace-path > .ui-icon { width: 16px; height: 16px; margin-right: 3px; color: var(--muted); }
.workspace-path button { min-width: 0; padding: 2px 3px; border: 0; border-radius: 4px; background: transparent; color: var(--muted); font: inherit; overflow-wrap: anywhere; text-align: left; cursor: pointer; }
.workspace-path button[aria-current] { color: var(--text-primary); font-weight: 600; }
.workspace-path button:hover:not(:disabled) { background: var(--bg-hover); }
.path-divider { color: var(--muted); }
.directory-content { height: 156px; overflow-y: auto; overscroll-behavior: contain; padding: 6px 0; }
.workspace-list { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 3px 6px; }
.workspace-entry { display: flex; align-items: center; gap: 9px; min-width: 0; min-height: 40px; padding: 9px 10px; border: 0; border-radius: 6px; background: transparent; color: var(--text-primary); text-align: left; font: inherit; font-size: 13px; cursor: pointer; }
.workspace-entry > .ui-icon { width: 16px; height: 16px; color: var(--muted); }
.workspace-entry:hover:not(:disabled) { background: var(--bg-subtle); }
.entry-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.workspace-entry .entry-arrow { width: 13px; height: 13px; margin-left: auto; transform: rotate(-90deg); opacity: 0; }
.workspace-entry:hover .entry-arrow { opacity: 1; }
@media (min-width: 769px) {
  .workspace-entry:focus-visible { background: var(--bg-subtle); }
  .workspace-entry:focus-visible .entry-arrow { opacity: 1; }
}
.directory-state { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 9px; min-height: 100%; padding: 16px; color: var(--muted); font-size: 13px; line-height: 1.5; text-align: center; overflow-wrap: anywhere; }
.directory-state > .ui-icon { width: 25px; height: 25px; }
.directory-state small { font-size: 12px; }
.new-directory-form { display: grid; grid-template-columns: minmax(0, 1fr) auto auto; gap: 6px; padding: 10px; background: var(--bg-subtle); border-bottom: 1px solid var(--border-soft); }
.new-directory-form input { min-width: 0; height: 34px; padding: 6px 10px; border: 1px solid var(--border-soft); border-radius: 6px; color: var(--text-primary); background: var(--bg-panel); font: inherit; font-size: 12px; }
.parallel-option { display: flex; flex: 0 0 auto; align-items: center; justify-content: space-between; gap: 16px; padding: 12px 16px; border-radius: 10px; background: var(--bg-subtle); cursor: pointer; }
.parallel-copy strong { font-size: 13px; font-weight: 500; }
.parallel-copy small { display: block; margin-top: 4px; color: var(--muted); font-size: 12px; line-height: 1.5; }
.parallel-switch { appearance: none; position: relative; flex: 0 0 auto; width: 34px; height: 20px; margin: 0; border: 1px solid var(--accent-border); border-radius: 20px; background: var(--accent-border); cursor: pointer; transition: background 0.15s; }
.parallel-switch::after { content: ''; position: absolute; left: 2px; top: 2px; width: 14px; height: 14px; border-radius: 50%; background: var(--bg-panel); box-shadow: 0 1px 2px #0002; transition: transform 0.15s; }
.parallel-switch:checked { background: var(--accent); border-color: var(--accent); }
.parallel-switch:checked::after { transform: translateX(14px); }
.directory-error { color: var(--danger); }
:where(button, input, select):focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
:where(button, input, select):disabled { cursor: not-allowed; opacity: 0.6; }
@media (max-width: 768px) {
  .inline-workspace { padding-top: 0; gap: 8px; }
  .workspace-heading { flex-wrap: wrap; gap: 4px 12px; }
  .workspace-header-actions { flex-wrap: wrap; margin-left: auto; }
  .workspace-header-actions .button { min-height: 44px; padding: 8px; }
  .workspace-path { max-height: 72px; overflow-y: auto; }
  .workspace-path button { min-height: 32px; }
  .workspace-list { grid-template-columns: 1fr; }
  .directory-content { height: clamp(88px, calc(var(--app-viewport-height, 100dvh) * .16), 156px); }
  .workspace-entry { min-height: 44px; font-size: 14px; }
  .workspace-entry .entry-arrow { opacity: 1; }
  .new-directory-form input { height: 44px; font-size: 16px; }
  .new-directory-form .button, .workspace-actions .button { min-height: 44px; }
  .workspace-hint { flex-basis: 100%; }
  .workspace-actions { justify-content: flex-end; }
}
@media (prefers-reduced-motion: reduce) {
  .parallel-switch, .parallel-switch::after { transition: none; }
  .creation-control, .agent-control-icon, .agent-control .agent-control-chevron, .agent-selection-menu button { transition: none; }
  .agent-selection-menu, .agent-control-label { animation: none; }
  .agent-control:active:not(:disabled) { transform: none; }
  .agent-control:hover:not(:disabled) .agent-control-icon { transform: none; }
}
</style>
