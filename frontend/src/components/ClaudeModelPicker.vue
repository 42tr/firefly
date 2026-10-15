<script setup>
import { computed, ref, useId } from 'vue'

const props = defineProps({
  modelValue: { type: String, default: '' },
  models: { type: Array, default: () => [] },
  defaultModel: { type: String, default: '' },
  disabled: Boolean,
})
const emit = defineEmits(['update:modelValue'])
const modelId = useId()
const customId = useId()
const custom = ref(false)
const CUSTOM = '__custom_model__'
const selection = computed({
  get: () => custom.value ? CUSTOM : props.modelValue,
  set: value => { custom.value = value === CUSTOM; if (!custom.value) emit('update:modelValue', value) },
})
</script>

<template>
  <div class="claude-model-picker">
    <label :for="modelId">模型</label>
    <select :id="modelId" v-model="selection" :disabled="disabled">
      <option value="">默认模型{{ defaultModel && defaultModel !== 'default' ? `（${defaultModel}）` : '' }}</option>
      <option v-if="modelValue && !models.includes(modelValue)" :value="modelValue">{{ modelValue }}</option>
      <option v-for="model in models" :key="model" :value="model">{{ model }}</option>
      <option :value="CUSTOM">自定义模型…</option>
    </select>
    <template v-if="custom">
      <label :for="customId">自定义模型名称</label>
      <input :id="customId" :value="modelValue" :disabled="disabled" type="text" maxlength="256"
        placeholder="模型别名或完整模型名称" @input="emit('update:modelValue', $event.target.value.trim())" />
    </template>
  </div>
</template>

<style scoped>
.claude-model-picker { display: grid; gap: 8px; min-width: 0; }
label { font-size: 13px; color: var(--text-secondary); }
select, input { width: 100%; min-width: 0; min-height: 42px; box-sizing: border-box; padding: 8px 12px; border: 1px solid var(--border-glass); border-radius: var(--radius-sm); background: var(--bg-subtle); color: var(--text-primary); font: inherit; font-size: 14px; }
select:focus-visible, input:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
@media (max-width: 768px) { select, input { min-height: 44px; font-size: 16px; } }
</style>
