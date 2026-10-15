<script setup>
import { computed, ref } from 'vue'
import ToolOutput from './ToolOutput.vue'
import { useLazyDetails } from '../composables/useLazyDetails.js'
const props = defineProps({ step: { type: Object, required: true }, conversationId: Number, jobId: String,
  parentId: { type: String, default: '' }, formatToolValue: Function, resolveImageUrl: Function })
const open = ref(false)
const url = () => {
  const conversation = props.conversationId || props.step.conversationId
  if (!conversation || !props.step.id) return ''
  const query = new URLSearchParams({ call_id: props.step.id, parent_id: props.parentId })
  const job = props.jobId || props.step.jobId
  if (job) query.set('job_id', job)
  else if (props.step.messageId) query.set('message_id', props.step.messageId)
  return `/api/assistant/conversations/${conversation}/tool-detail?${query}`
}
const { detail, loading, error, retry } = useLazyDetails(url, () => props.step.status, open)
const format = value => props.formatToolValue ? props.formatToolValue(value) : JSON.stringify(value, null, 2)
const hasInput = computed(() => detail.value?.input != null)
</script>
<template>
  <details class="lazy-tool" @toggle="open = $event.target.open">
    <summary class="lazy-tool-header"><slot name="summary"><code>{{ step.name || step.agent || '工具调用' }}</code></slot></summary>
    <div v-if="open" class="lazy-tool-body" :aria-busy="loading">
      <p v-if="loading && !detail" role="status">正在加载详情…</p>
      <p v-if="error" class="lazy-tool-error" role="alert">{{ error }} <button type="button" @click="retry">重试</button></p>
      <div v-if="hasInput" class="run-section"><div class="run-section-label">输入</div><pre>{{ format(detail.input) }}</pre></div>
      <slot></slot>
      <div v-if="detail?.output != null" class="run-section"><div class="run-section-label">输出</div><ToolOutput :value="detail.output" :resolve-image-url="resolveImageUrl" /></div>
      <p v-else-if="detail" class="lazy-tool-empty">{{ step.status === 'running' ? '正在执行，等待输出…' : '暂无输出' }}</p>
    </div>
  </details>
</template>
<style scoped>
.lazy-tool { min-width: 0; overflow: hidden; border: 1px solid var(--border-soft); border-radius: 8px; background: var(--bg-panel, #fff); }
.lazy-tool-header { display: flex; align-items: center; gap: 10px; min-height: 40px; padding: 8px 12px; cursor: pointer; list-style: none; font-size: 12px; }
.lazy-tool-header::-webkit-details-marker { display: none; }
.lazy-tool-header::after { content: '›'; margin-left: auto; color: var(--muted); font-size: 17px; transition: transform .15s; }
.lazy-tool[open] > summary::after { transform: rotate(90deg); }
.lazy-tool-header:hover, .lazy-tool[open] > summary { background: var(--bg-subtle); }
.lazy-tool-header:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
.lazy-tool-body { border-top: 1px solid var(--border-soft); }
.run-section { padding: 10px 12px; }
.run-section + .run-section { border-top: 1px solid var(--border-soft); }
.run-section-label, .lazy-tool-empty { color: var(--muted); font-size: 12px; }
pre { max-height: 280px; overflow: auto; margin: 6px 0 0; padding: 10px; border-radius: 6px; background: var(--bg-subtle); font-size: 12px; white-space: pre-wrap; overflow-wrap: anywhere; }
p { padding: 0 12px; font-size: 12px; }
.lazy-tool-error { color: var(--danger); }
button { margin-left: 6px; cursor: pointer; }
</style>
