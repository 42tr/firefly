<script setup>
import { useId } from 'vue'
import ClaudeModelPicker from './ClaudeModelPicker.vue'

defineProps({
  modelValue: { type: String, default: '' },
  provider: { type: String, default: '' },
  reasoningLevel: { type: String, default: '' },
  providers: { type: Array, default: () => [] },
  models: { type: Array, default: () => [] },
  reasoningLevels: { type: Array, default: () => [] },
  disabled: Boolean,
})
const emit = defineEmits(['update:modelValue', 'update:provider', 'update:reasoningLevel', 'change-provider'])
const providerId = useId()
const reasoningId = useId()
const selectProvider = event => {
  emit('update:provider', event.target.value)
  emit('change-provider')
}
</script>

<template>
  <div class="claude-settings-fields">
    <div class="setting-field">
      <label :for="providerId">API 平台</label>
      <select :id="providerId" :value="provider" :disabled="disabled" @change="selectProvider">
        <option v-if="!provider" value="" disabled>请选择平台</option>
        <option value="cli">Claude CLI</option>
        <option v-if="provider && provider !== 'cli' && !providers.some(item => item.name === provider)" :value="provider">{{ provider }}</option>
        <option v-for="item in providers" :key="item.name" :value="item.name">{{ item.name }}</option>
      </select>
    </div>
    <ClaudeModelPicker :key="provider" :model-value="modelValue" :models="models" :disabled="disabled" @update:model-value="emit('update:modelValue', $event)" />
    <div class="setting-field">
      <label :for="reasoningId">推理级别</label>
      <select :id="reasoningId" :value="reasoningLevel" :disabled="disabled" @change="emit('update:reasoningLevel', $event.target.value)">
        <option v-if="!reasoningLevel" value="" disabled>请选择推理级别</option>
        <option v-if="reasoningLevel && !reasoningLevels.includes(reasoningLevel)" :value="reasoningLevel">{{ reasoningLevel }}</option>
        <option v-for="level in reasoningLevels" :key="level" :value="level">{{ level }}</option>
      </select>
    </div>
    <p class="effort-hint">可用推理级别取决于模型和 API 平台。</p>
  </div>
</template>

<style scoped>
.claude-settings-fields { display: grid; gap: 16px; min-width: 0; }
.setting-field { display: grid; gap: 8px; min-width: 0; }
label { font-size: 13px; color: var(--text-secondary); }
select { width: 100%; min-width: 0; min-height: 42px; box-sizing: border-box; padding: 8px 12px; border: 1px solid var(--border-glass); border-radius: var(--radius-sm); background: var(--bg-subtle); color: var(--text-primary); font: inherit; font-size: 14px; }
select:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
.effort-hint { margin: 0; color: var(--muted); font-size: 12px; line-height: 1.5; }
@media (max-width: 768px) { select { min-height: 44px; font-size: 16px; } }
</style>
