<script setup>
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'
import { activeTask, taskTree } from '../composables/taskTree.js'
import { useConversationTasks } from '../composables/useConversationTasks.js'
import WorkflowNode from './WorkflowNode.vue'
const props = defineProps({ conversationId: Number, jobId: String, replyStatus: String, active: { type: Boolean, default: true } })
const { tasks, error, refresh } = useConversationTasks(
  () => props.conversationId, () => props.jobId, () => props.replyStatus, () => props.active,
)
const cancelling = ref(new Set()), clock = ref(Date.now())
const expanded = ref(true)
const ownTasks = computed(() => tasks.value.filter(task => task.generation_job_id === props.jobId))
const roots = computed(() => taskTree(ownTasks.value))
const background = computed(() => ownTasks.value.filter(task => task.detached && activeTask(task)))
const pending = computed(() => ownTasks.value.some(activeTask))
const replyActive = computed(() => ['running', 'queued', 'cancelling'].includes(props.replyStatus))
const summary = computed(() => background.value.length ? `后台任务 ${background.value.length}`
  : pending.value || replyActive.value ? '任务执行中' : ownTasks.value.some(task => ['failed', 'unknown', 'timed_out'].includes(task.status)) ? '任务有异常' : '任务已结束')
let ticker
async function cancel(ids) {
  cancelling.value = new Set([...cancelling.value, ...ids])
  try {
    for (let offset = 0; offset < ids.length; offset += 64) {
      const response = await apiFetch(`/api/assistant/conversations/${props.conversationId}/tasks/cancel`, {
        method: 'POST', credentials: 'include', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ task_ids: ids.slice(offset, offset + 64) }),
      })
      if (!response.ok) throw new Error('取消任务失败，请重试')
    }
    refresh()
  } catch (cause) { error.value = cause.message }
  finally { cancelling.value = new Set([...cancelling.value].filter(id => !ids.includes(id))) }
}
watch(() => [pending.value, props.active], () => {
  clearInterval(ticker)
  clock.value = Date.now()
  if (props.active && pending.value) ticker = setInterval(() => { clock.value = Date.now() }, 1000)
}, { immediate: true })
onBeforeUnmount(() => { clearInterval(ticker) })
</script>
<template>
  <section v-if="ownTasks.length || replyActive || error" class="workflow" aria-label="执行工作流程">
    <div class="workflow-header"><strong>执行流程</strong><span class="workflow-summary" role="status">{{ summary }}</span>
      <button v-if="background.length" class="workflow-cancel" @click="cancel(background.map(task => task.id))">取消本回答的后台任务</button>
    </div>
    <p v-if="error" class="workflow-error">{{ error }}</p>
    <div class="workflow-root">
      <button class="workflow-toggle" :aria-expanded="expanded" :aria-label="expanded ? '收起执行流程' : '展开执行流程'" @click="expanded = !expanded">
        <span aria-hidden="true">{{ expanded ? '▾' : '▸' }}</span>
        <span class="workflow-icon" :class="{ succeeded: replyStatus === 'completed' }" aria-hidden="true">{{ ({ completed: '✓', failed: '!', cancelled: '−' })[replyStatus] || '◌' }}</span>
        <span>Firefly agent</span>
      </button>
      <span class="workflow-time">{{ replyActive ? '正在处理' : '回答已结束' }}</span>
    </div>
    <ul v-if="expanded" class="workflow-children"><WorkflowNode v-for="node in roots" :key="node.id" :node="node" :conversation-id="conversationId" :active="active" :clock="clock" :cancelling="cancelling" @cancel="cancel" /></ul>
  </section>
</template>
<style>
.workflow { border: 1px solid var(--border-color, #d6dde2); border-radius: 12px; padding: 14px 16px; font-size: 13px; min-width: 0; }
.workflow-header, .workflow-row, .workflow-root { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.workflow-header { margin-bottom: 14px; }
.workflow-summary { color: var(--muted); flex: 1; }
.workflow-root { font-weight: 600; }
.workflow-root .workflow-toggle { font: inherit; }
.workflow-children { list-style: none; padding: 0 0 0 22px; margin: 0 0 0 7px; }
.workflow-node { position: relative; padding: 10px 0 2px; }
.workflow-node::before { content: ''; position: absolute; left: -22px; top: 0; bottom: 0; border-left: 1px solid var(--border-color, #c8d2d9); }
.workflow-node::after { content: ''; position: absolute; left: -22px; top: 23px; width: 17px; border-top: 1px solid var(--border-color, #c8d2d9); }
.workflow-node:last-child::before { bottom: auto; height: 23px; }
.workflow-toggle { display: inline-flex; align-items: center; gap: 7px; min-width: 0; max-width: 100%; text-align: left; background: transparent; color: inherit; border: 0; padding: 3px 0; cursor: pointer; }
.workflow-toggle:focus-visible, .workflow-cancel:focus-visible { outline: 2px solid #4c8baa; outline-offset: 3px; }
.workflow-title { overflow-wrap: anywhere; }
.workflow-icon { width: 16px; text-align: center; }
.workflow-state { font-size: 12px; color: var(--muted); }
.workflow-time { font-size: 11px; color: var(--muted); font-weight: 400; }
.workflow-agent-summary { flex: 1; min-width: 0; font-size: 12px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.workflow-cancel { margin-left: auto; padding: 3px 7px; border: 1px solid var(--border-color, #d6dde2); border-radius: 5px; background: transparent; color: inherit; font-size: 11px; cursor: pointer; }
.workflow-cancel:disabled { opacity: .5; cursor: wait; }
.workflow-icon.succeeded { color: #398368; }
.workflow-state.failed, .workflow-state.timed_out, .workflow-state.unknown, .workflow-error { color: #b25238; }
.workflow-detail { margin: 8px 0 4px 24px; overflow-wrap: anywhere; }
.workflow-detail p { margin: 6px 0; font-size: 12px; }
.workflow-command, .workflow-output { white-space: pre-wrap; overflow-wrap: anywhere; background: var(--bg-secondary, #f0f3f5); color: inherit; border-radius: 7px; padding: 10px; font-size: 12px; max-height: 300px; overflow: auto; }
.workflow-detail summary { cursor: pointer; font-size: 12px; }
@media (max-width: 600px) { .workflow { padding: 12px 8px; } .workflow-children { padding-left: 16px; } .workflow-node::before, .workflow-node::after { left: -16px; } .workflow-node::after { width: 12px; } .workflow-agent-summary { flex-basis: 100%; white-space: normal; overflow-wrap: anywhere; padding-left: 24px; } }
</style>
