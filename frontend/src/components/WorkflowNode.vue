<script setup>
import { computed, ref } from 'vue'
import { useLazyDetails } from '../composables/useLazyDetails.js'
import { activeTask, activeBranch, taskStatus } from '../composables/taskTree.js'
import { formatAppDateTime } from '../composables/appTimezone.js'
const props = defineProps({ node: { type: Object, required: true }, conversationId: Number, active: { type: Boolean, default: true }, clock: Number, cancelling: Object })
const emit = defineEmits(['cancel'])
const expanded = ref(props.node.kind === 'handoff' || !!props.node.children.length)
const detailExpanded = ref(false)
const isTool = computed(() => ['tool', 'handoff'].includes(props.node.kind))
const visible = computed(() => expanded.value && detailExpanded.value && props.active)
function toggle() {
  expanded.value = !expanded.value
  detailExpanded.value = expanded.value
}
const { detail, loading, error, retry } = useLazyDetails(
  () => {
    if (!props.conversationId || !props.node.id) return ''
    const base = `/api/assistant/conversations/${props.conversationId}`
    return isTool.value ? `${base}/tool-detail?${new URLSearchParams({ job_id: props.node.generation_job_id, call_id: props.node.call_id, parent_id: props.node.parent_call_id || '' })}`
      : `${base}/tasks/${encodeURIComponent(props.node.id)}`
  },
  () => props.node.status, visible,
)
const argumentsValue = computed(() => isTool.value ? detail.value?.input : detail.value?.arguments)
const resultValue = computed(() => isTool.value ? detail.value?.output : detail.value?.result)
const command = computed(() => argumentsValue.value?.command || argumentsValue.value?.cmd)
const elapsed = computed(() => {
  const start = props.node.started_at ?? props.node.created_at
  const end = props.node.ended_at ?? (activeTask(props.node) ? props.clock : null)
  if (start == null || end == null) return '耗时未记录'
  const seconds = Math.max(0, Math.floor((end - start) / 1000))
  return `${props.node.started_at != null ? '执行' : '等待'} ${seconds >= 60 ? `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒` : `${seconds} 秒`}`
})
const agentSummary = computed(() => {
  if (props.node.kind !== 'handoff') return ''
  if (props.node.summary) return props.node.summary
  const completed = []
  function collect(node) {
    for (const child of node.children || []) {
      if (child.status === 'succeeded') completed.push(child.title)
      collect(child)
    }
  }
  collect(props.node)
  if (!completed.length) return ''
  return `已完成 ${completed.length} 个步骤：${completed.slice(0, 2).join('、')}${completed.length > 2 ? '等' : ''}`
})
const icon = computed(() => ({ succeeded: '✓', failed: '!', unknown: '?', timed_out: '!', cancelled: '−', queued: '○' }[props.node.status] || '◌'))
</script>
<template>
  <li class="workflow-node" :data-task-id="node.id">
    <div class="workflow-row">
      <button class="workflow-toggle" :aria-expanded="expanded" @click="toggle">
        <span>{{ expanded ? '▾' : '▸' }}</span><span class="workflow-icon" :class="node.status">{{ icon }}</span>
        <span class="workflow-title">{{ node.title }}</span>
      </button>
      <span class="workflow-state" :class="node.status">{{ taskStatus(node) }}</span><span class="workflow-time">{{ elapsed }}</span>
      <span v-if="agentSummary" class="workflow-agent-summary" :title="agentSummary">{{ agentSummary }}</span>
      <button v-if="!isTool && activeBranch(node) && node.status !== 'cancelling'" class="workflow-cancel"
        :disabled="cancelling?.has(node.id)" @click="emit('cancel', [node.id])">{{ cancelling?.has(node.id) ? '取消中…' : node.children.length ? '取消此分支' : '取消' }}</button>
    </div>
    <div v-if="expanded && detailExpanded" class="workflow-detail">
      <p v-if="loading && !detail" role="status">正在加载任务详情…</p>
      <p v-if="error" class="workflow-error" role="alert">{{ error }} <button type="button" @click="retry">重试</button></p>
      <p v-if="node.status === 'queued'">{{ isTool ? '等待 agent 执行此步骤。' : '等待工作目录释放或串行工具执行结束；不设排队超时。' }}</p>
      <p v-if="argumentsValue?.cwd">工作目录：<code>{{ argumentsValue.cwd }}</code></p>
      <pre v-if="command" class="workflow-command">{{ command }}</pre>
      <details v-else-if="argumentsValue != null"><summary>输入参数</summary><pre class="workflow-output">{{ JSON.stringify(argumentsValue, null, 2) }}</pre></details>
      <p v-if="node.deadline_at && activeTask(node)">执行期限：{{ formatAppDateTime(node.deadline_at) }}</p>
      <p v-if="detail?.error" class="workflow-error">{{ detail?.error }}</p>
      <details v-if="!isTool && detail?.output">
        <summary>命令输出 <span class="workflow-time">（显示最近 100,000 字符）</span></summary>
        <pre class="workflow-output" tabindex="0">{{ detail.output }}</pre>
      </details>
      <details v-if="resultValue != null"><summary>{{ node.kind === 'handoff' ? '子 agent 返回结果' : '执行结果' }}</summary><pre class="workflow-output">{{ typeof resultValue === 'string' ? resultValue : JSON.stringify(resultValue, null, 2) }}</pre></details>
      <p v-if="resultValue == null && !detail?.error && !detail?.output && node.status === 'running'">{{ node.kind === 'handoff' ? '子 agent 正在处理，执行步骤实时显示在下方。' : '任务正在执行，等待输出。' }}</p>
    </div>
    <ul v-if="expanded && node.children.length" class="workflow-children">
      <WorkflowNode v-for="child in node.children" :key="child.id" :node="child" :conversation-id="conversationId" :active="active" :clock="clock" :cancelling="cancelling" @cancel="emit('cancel', $event)" />
    </ul>
  </li>
</template>
