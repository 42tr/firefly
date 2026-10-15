<script setup>
import { computed, ref } from 'vue'
import { apiFetch } from '@/utils/http'

const props = defineProps({ conversation: { type: Object, required: true } })
const emit = defineEmits(['retried', 'create'])
const busy = ref(false)
const error = ref('')
const stateLabel = computed(() => ({
  pending: '首次发送后准备独立工作目录', preparing: '正在准备独立工作目录…', ready: '独立工作目录已就绪',
  failed: '工作目录准备失败', cancelled: '工作目录准备已取消', missing: '副本不可用，请检查目录或主机连接',
}[props.conversation.workspace_state] || '首次发送后准备独立工作目录'))
const retry = async () => {
  if (busy.value || props.conversation.is_archived) return
  busy.value = true
  error.value = ''
  try {
    const response = await apiFetch(`/api/assistant/conversations/${props.conversation.id}/workspace/retry`, {
      method: 'POST', credentials: 'include',
    })
    if (!response.ok) throw new Error((await response.text()) || '重试失败')
    emit('retried', props.conversation.id)
  } catch (err) {
    error.value = err.message
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="parallel-status">
    <details>
      <summary><span class="parallel-badge">并行</span> <span role="status">{{ conversation.workspace_state === 'preparing' ? (conversation.workspace_progress || stateLabel) : stateLabel }}</span></summary>
      <p>来源：<code>{{ conversation.source_cwd ?? conversation.cwd ?? 'workspace/' }}</code></p>
      <p v-if="conversation.id">实际目录：<code>{{ conversation.cwd }}</code></p>
      <p>副本在删除对话后仍会保留。端口和外部服务仍由当前主机共享。</p>
      <button v-if="conversation.workspace_state === 'ready'" class="button ghost small" type="button" @click="emit('create', conversation)">从此副本新建对话</button>
    </details>
    <p v-if="conversation.workspace_error || error" role="alert">{{ error || conversation.workspace_error }}</p>
    <button v-if="!conversation.is_archived && ['failed', 'cancelled', 'missing'].includes(conversation.workspace_state)" class="button ghost small" type="button" :disabled="busy" @click="retry">{{ busy ? '正在重试…' : conversation.workspace_state === 'missing' ? '重新检查副本' : '重试目录准备' }}</button>
  </div>
</template>

<style scoped>
.parallel-status { padding: 8px 16px; border-bottom: 1px solid var(--border-soft); font-size: 12px; color: var(--text-secondary); }
summary { cursor: pointer; line-height: 1.6; }
.parallel-badge { color: var(--text-primary); font-weight: 600; }
p { margin: 6px 0; overflow-wrap: anywhere; }
code { font-size: 11px; }
</style>
