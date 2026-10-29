<script setup>
import { computed, ref } from 'vue'
import { apiFetch } from '@/utils/http'
import { activeTask } from '../composables/taskState.js'
import { useConversationTasks } from '../composables/useConversationTasks.js'

const props = defineProps({ conversationId: Number, jobId: String, replyStatus: String, active: { type: Boolean, default: true } })
const { tasks, error, refresh } = useConversationTasks(
  () => props.conversationId, () => props.jobId, () => props.replyStatus, () => props.active,
)
const cancelling = ref(false)
const background = computed(() => tasks.value.filter(task =>
  task.generation_job_id === props.jobId && task.detached && activeTask(task)))
async function cancel() {
  const ids = background.value.map(task => task.id)
  if (!ids.length || cancelling.value) return
  cancelling.value = true
  try {
    for (let offset = 0; offset < ids.length; offset += 64) {
      const response = await apiFetch(`/api/assistant/conversations/${props.conversationId}/tasks/cancel`, {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ task_ids: ids.slice(offset, offset + 64) }),
      })
      if (!response.ok) throw new Error('取消任务失败，请重试')
    }
    refresh()
  } catch (cause) {
    error.value = cause.message
  } finally {
    cancelling.value = false
  }
}
</script>

<template>
  <div v-if="background.length" class="background-task-bar">
    <span class="background-task-label">后台任务 {{ background.length }}</span>
    <button type="button" class="background-task-cancel" :disabled="cancelling" @click="cancel">取消</button>
    <span v-if="error" class="background-task-error" role="status">{{ error }}</span>
  </div>
</template>

<style scoped>
.background-task-bar { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; min-width: 0; margin: 4px 0 0; font-size: 12px; line-height: 1.4; color: var(--muted); }
.background-task-label { overflow-wrap: anywhere; }
.background-task-cancel { padding: 2px 8px; border: 1px solid var(--border-soft, #d6dde2); border-radius: 6px; background: transparent; color: inherit; font: inherit; font-size: 11px; cursor: pointer; }
.background-task-cancel:hover { color: var(--text, inherit); }
.background-task-cancel:focus-visible { outline: 2px solid var(--focus-ring, #4c8baa); outline-offset: 2px; }
.background-task-cancel:disabled { opacity: .5; cursor: wait; }
.background-task-error { color: #b25238; overflow-wrap: anywhere; }
</style>
