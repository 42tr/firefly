<script setup>
import { onBeforeUnmount, ref, watch } from 'vue'

const props = defineProps({ text: String, label: { type: String, default: '复制' } })
const state = ref('')
let timer
let revision = 0
const reset = () => {
  revision += 1
  clearTimeout(timer)
  state.value = ''
}
watch(() => props.text, reset)
onBeforeUnmount(reset)
const copy = async () => {
  reset()
  const current = revision
  state.value = '复制中…'
  try {
    if (!navigator.clipboard?.writeText) throw new Error('剪贴板不可用')
    await navigator.clipboard.writeText(props.text || '')
    if (current !== revision) return
    state.value = '已复制'
  } catch {
    if (current !== revision) return
    state.value = '复制失败，请手动选择文本'
  }
  timer = setTimeout(reset, 3000)
}
</script>

<template>
  <button type="button" class="copy-button" :aria-label="state || label" :disabled="state === '复制中…'" @click="copy">
    <span role="status">{{ state || label }}</span>
  </button>
</template>

<style scoped>
.copy-button { padding: 5px 8px; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--muted); font: inherit; font-size: 12px; cursor: pointer; text-align: left; }
.copy-button:hover { background: var(--bg-hover); color: var(--text-primary); }
@media (max-width: 768px), (pointer: coarse) { .copy-button { min-height: 44px; } }
</style>
