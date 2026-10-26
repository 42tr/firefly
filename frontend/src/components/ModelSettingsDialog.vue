<script setup>
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from 'vue'
import { apiFetch } from '@/utils/http'
import { useCodexModels } from '@/composables/useCodexModels'
import { useClaudeModels } from '@/composables/useClaudeModels'
import ClaudeSettingsFields from './ClaudeSettingsFields.vue'
import UiIcon from './UiIcon.vue'

const props = defineProps({
  open: Boolean,
  conversation: { type: Object, default: null },
})
const emit = defineEmits(['close', 'saved'])
const titleId = useId()
const providerId = useId()
const modelId = useId()
const reasoningId = useId()
const dialog = ref(null)
const saving = ref(false)
const error = ref('')
const providerResolved = ref(false)
const agent = computed(() => props.conversation?.agent)
const isClaude = computed(() => agent.value === 'claude')
const {
  model: claudeModel, modelOptions: claudeModelOptions,
  provider: claudeProvider, providers: claudeProviders,
  reasoningLevel: claudeReasoningLevel, reasoningOptions: claudeReasoningOptions,
  changeProvider: changeClaudeProvider,
  modelsLoading: claudeModelsLoading, modelsError: claudeModelsError,
  loadModels: loadClaudeModels, cancelModelLoad: cancelClaudeModelLoad,
} = useClaudeModels()
const {
  providers, modelsLoading, modelsError, defaultProvider, provider, model, reasoningLevel,
  loadProviders, modelOptions, providerModelsError, reasoningOptions, changeProvider, cancelModelLoad, resolveDefaults,
} = useCodexModels(agent)
const loading = computed(() => isClaude.value ? claudeModelsLoading.value : modelsLoading.value)
const blockedReason = computed(() => {
  if (!props.conversation || !['codex', 'claude'].includes(agent.value)) return '该对话无法修改模型设置'
  if (props.conversation.is_archived) return '请先取消归档，再修改模型设置'
  return ''
})
let previousFocus = null
let opening = 0
const close = () => { if (!saving.value) emit('close') }
const releaseFocus = () => {
  previousFocus?.focus?.()
  previousFocus = null
}
const onKeydown = event => {
  if (event.isComposing) return
  if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(); return }
  if (event.key !== 'Tab') return
  const items = [...(dialog.value?.querySelectorAll('button:not([disabled]), input:not([disabled]), select:not([disabled])') || [])]
  const first = items[0]
  const last = items.at(-1)
  if (!first) { event.preventDefault(); dialog.value?.focus(); return }
  if (event.shiftKey && (document.activeElement === first || document.activeElement === dialog.value)) {
    event.preventDefault(); last.focus()
  } else if (!event.shiftKey && (document.activeElement === last || document.activeElement === dialog.value)) {
    event.preventDefault(); first.focus()
  }
}

const loadCurrentProviders = async (current, token) => {
  const loaded = await loadProviders(false, true)
  if (token !== opening) return
  providerResolved.value = !!current?.codex_provider || !!loaded
  if (!current?.codex_provider && loaded) provider.value = defaultProvider.value
  if (loaded) resolveDefaults()
}
const selectProvider = () => {
  providerResolved.value = true
  changeProvider()
}

watch([() => props.open, () => props.conversation?.id], async ([open]) => {
  const token = ++opening
  cancelModelLoad()
  cancelClaudeModelLoad()
  error.value = ''
  if (!open) { releaseFocus(); return }
  previousFocus ||= document.activeElement
  const current = props.conversation
  claudeModel.value = current?.claude_model || ''
  claudeProvider.value = current?.claude_provider || ''
  claudeReasoningLevel.value = current?.claude_reasoning_level || ''
  providerResolved.value = !!current?.codex_provider
  provider.value = current?.codex_provider === 'openai' ? '' : current?.codex_provider || ''
  model.value = current?.codex_model || ''
  reasoningLevel.value = current?.codex_reasoning_level || ''
  // An inherited provider must be resolved before we display or save the selection.
  const loading = isClaude.value ? loadClaudeModels() : loadCurrentProviders(current, token)
  await nextTick()
  if (token === opening) dialog.value?.focus()
  await loading
}, { immediate: true })
onBeforeUnmount(() => { opening += 1; cancelClaudeModelLoad(); releaseFocus() })

const save = async () => {
  if (saving.value || loading.value || (!isClaude.value && !providerResolved.value) || blockedReason.value) return
  const id = props.conversation.id
  saving.value = true
  error.value = ''
  try {
    const response = await apiFetch(`/api/assistant/conversations/${id}/${agent.value}-settings`, {
      method: 'PATCH', credentials: 'include', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(isClaude.value ? { provider: claudeProvider.value || null, model: claudeModel.value.trim() || null, reasoning_level: claudeReasoningLevel.value || null } : {
        provider: provider.value || 'openai', model: model.value.trim() || null,
        reasoning_level: reasoningLevel.value || null,
      }),
    })
    if (!response.ok) {
      const text = await response.text()
      let message = text
      try { message = JSON.parse(text)?.error || text } catch { /* Keep plain error text. */ }
      throw new Error(message || '保存模型设置失败')
    }
    const updated = await response.json()
    emit('saved', updated)
    if (props.open && props.conversation?.id === id) emit('close')
  } catch (err) {
    if (props.open && props.conversation?.id === id) error.value = err.message || '保存模型设置失败'
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div v-if="open" class="codex-settings-backdrop" @click.self="close">
    <section ref="dialog" class="codex-settings-dialog" role="dialog" aria-modal="true" :aria-labelledby="titleId" tabindex="-1" @keydown="onKeydown">
      <div class="codex-settings-header">
        <h2 :id="titleId">模型设置</h2>
        <button class="button ghost icon" type="button" aria-label="关闭模型设置" :disabled="saving" @click="close"><UiIcon name="x" /></button>
      </div>
      <p class="settings-hint">保存后发送的新消息使用新设置，运行和排队的消息保持原设置。</p>
      <form @submit.prevent="save">
        <fieldset :disabled="saving || loading || !!blockedReason">
          <ClaudeSettingsFields v-if="isClaude" v-model="claudeModel" v-model:provider="claudeProvider" v-model:reasoning-level="claudeReasoningLevel" :providers="claudeProviders" :reasoning-levels="claudeReasoningOptions" @change-provider="changeClaudeProvider" :models="claudeModelOptions" :disabled="saving || loading || !!blockedReason" />
          <template v-else>
          <label :for="providerId">API 平台</label>
          <select :id="providerId" v-model="provider" @change="selectProvider">
            <option value="">openai</option>
            <option v-if="provider && !providers.some(item => item.name === provider)" :value="provider">{{ provider }}</option>
            <option v-for="item in providers.filter(item => item.name !== 'openai')" :key="item.name" :value="item.name">{{ item.name }}</option>
          </select>
          <label :for="modelId">模型</label>
          <select v-if="modelOptions.length" :id="modelId" v-model="model">
            <option v-if="!model" value="" disabled>请选择模型</option>
            <option v-if="model && !modelOptions.includes(model)" :value="model">{{ model }}</option>
            <option v-for="item in modelOptions" :key="item" :value="item">{{ item }}</option>
          </select>
          <input v-else :id="modelId" v-model.trim="model" type="text" maxlength="256" placeholder="输入模型名称" />
          <template v-if="reasoningOptions.length">
            <label :for="reasoningId">推理级别</label>
            <select :id="reasoningId" v-model="reasoningLevel">
              <option v-if="!reasoningLevel" value="" disabled>请选择推理级别</option>
              <option v-for="level in reasoningOptions" :key="level" :value="level">{{ level }}</option>
            </select>
          </template>
          </template>
        </fieldset>
        <p v-if="loading" class="settings-hint" role="status">正在读取模型列表…</p>
        <p v-else-if="isClaude ? claudeModelsError : modelsError || providerModelsError" class="settings-hint" role="status">{{ isClaude ? claudeModelsError : modelsError || providerModelsError }}</p>
        <button v-if="isClaude && claudeModelsError" class="button ghost small" type="button" :disabled="loading || saving" @click="loadClaudeModels">重试读取配置</button>
        <p v-if="!isClaude && !modelsLoading && !providerResolved" class="settings-error" role="alert">无法确定当前默认平台，请重试读取配置。<button class="button ghost small" type="button" @click="loadCurrentProviders(conversation, opening)">重试</button></p>
        <p v-if="blockedReason || error" class="settings-error" role="alert">{{ blockedReason || error }}</p>
        <div class="codex-settings-actions">
          <button class="button ghost" type="button" :disabled="saving" @click="close">取消</button>
          <button class="button primary" type="submit" :disabled="saving || loading || (!isClaude && !providerResolved) || !!blockedReason">{{ saving ? '保存中…' : '保存' }}</button>
        </div>
      </form>
    </section>
  </div>
</template>

<style scoped>
.codex-settings-backdrop { position: fixed; inset: 0; z-index: 1100; display: flex; align-items: center; justify-content: center; padding: 20px; background: rgba(0, 0, 0, .45); }
.codex-settings-dialog { width: min(440px, 100%); max-height: calc(100dvh - 40px); overflow: auto; padding: 24px; border: 1px solid var(--border-glass); border-radius: var(--radius); background: var(--bg-panel); box-shadow: var(--shadow-float); }
.codex-settings-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
h2 { margin: 0; font-size: 18px; }
.settings-hint { font-size: 12px; line-height: 1.6; color: var(--muted); }
fieldset { display: grid; gap: 8px; border: 0; padding: 0; margin: 20px 0; min-width: 0; }
label { font-size: 13px; color: var(--text-secondary); }
label:not(:first-child) { margin-top: 8px; }
select, input { width: 100%; min-width: 0; min-height: 42px; box-sizing: border-box; padding: 8px 12px; border: 1px solid var(--border-glass); border-radius: var(--radius-sm); background: var(--bg-subtle); color: var(--text-primary); font: inherit; font-size: 14px; }
select:focus-visible, input:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
.settings-error { color: var(--danger, #f87171); font-size: 13px; line-height: 1.6; }
.codex-settings-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 24px; }
</style>
