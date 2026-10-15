<script setup>
import { computed, ref } from 'vue'
import ToolCallDetails from './ToolCallDetails.vue'
import { isCommandRun } from '../composables/commandGroups.js'

const props = defineProps({
  conversationId: Number,
  jobId: String,
  commands: { type: Array, required: true },
  toolStatusText: { type: Function, required: true },
  formatToolValue: { type: Function, required: true },
  resolveImageUrl: { type: Function, default: undefined },
})

const open = ref(false)
const runningCount = computed(() => props.commands.filter(command => command.status === 'running').length)
const errorCount = computed(() => props.commands.filter(command => command.status === 'error').length)
const completedCount = computed(() => props.commands.filter(command => command.status === 'completed').length)
const fileChangeCount = computed(() => props.commands.filter(command => command.name === 'file_change').length)
const commandCount = computed(() => props.commands.filter(isCommandRun).length)
const toolCount = computed(() => props.commands.length - commandCount.value - fileChangeCount.value)
const summary = computed(() => {
  const label = commandCount.value === props.commands.length ? 'command'
    : fileChangeCount.value === props.commands.length ? 'file change' : 'operation'
  return `${props.commands.length} ${label}${props.commands.length === 1 ? '' : 's'}`
})
const breakdown = computed(() => [
  commandCount.value ? `${commandCount.value} 条命令` : '',
  fileChangeCount.value ? `${fileChangeCount.value} 次文件变更` : '',
  toolCount.value ? `${toolCount.value} 次工具调用` : '',
].filter(Boolean).join(' · '))
const status = computed(() => runningCount.value ? 'running' : errorCount.value ? 'error' : completedCount.value === props.commands.length ? 'completed' : 'idle')
</script>

<template>
  <details class="command-group" :class="status" @toggle="open = $event.target.open">
    <summary class="command-group-header">
      <span class="chevron" aria-hidden="true"></span>
      <span>{{ summary }}</span>
      <span v-if="runningCount" class="command-status">· Running</span>
    </summary>
    <div v-if="open" class="command-body">
      <div class="command-toolbar">
        <span>{{ breakdown }}<span v-if="completedCount"> · {{ completedCount }} 项完成</span></span>
        <span>点击单项查看详情</span>
      </div>
      <ToolCallDetails v-for="(command, index) in commands" :key="command.id ?? index" class="command-entry"
        :step="command" :conversation-id="conversationId" :job-id="jobId" :format-tool-value="formatToolValue" :resolve-image-url="resolveImageUrl">
        <template #summary>
          <span class="command-number">{{ String(index + 1).padStart(2, '0') }}</span>
          <code class="command-line">{{ command.name || '工具调用' }}</code>
          <span class="entry-status" :class="command.status" role="img" :aria-label="toolStatusText(command.status)" :title="toolStatusText(command.status)"><span class="status-dot" aria-hidden="true"></span></span>
        </template>
      </ToolCallDetails>
    </div>
  </details>
</template>

<style scoped>
.command-group {
  min-width: 0;
}
summary { list-style: none; cursor: pointer; }
summary::-webkit-details-marker { display: none; }
summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -3px; }
.command-group-header {
  display: inline-flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  max-width: 100%;
  min-height: 28px;
  padding: 3px 4px;
  border-radius: 4px;
  color: var(--muted);
  font-size: 12px;
  line-height: 20px;
  transition: color 0.15s ease, background 0.15s ease;
}
.command-group-header:hover { color: var(--text-primary); background: var(--bg-subtle); }
@media (min-width: 769px) {
  .command-group-header:focus-visible { color: var(--text-primary); background: var(--bg-subtle); }
}
.command-line { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font: 12px/1.5 var(--font-mono); }
.command-status { font-size: 11px; }
.status-dot { display: inline-block; width: 6px; height: 6px; border-radius: 50%; background: currentColor; }
.entry-status.completed { color: var(--success-text); }
.entry-status.running { color: var(--warning-text); }
.entry-status.error { color: var(--error-text); }
.running > .status-dot { animation: command-pulse 1.4s ease-in-out infinite; }
.chevron { width: 6px; height: 6px; flex-shrink: 0; border-right: 1.5px solid var(--muted); border-bottom: 1.5px solid var(--muted); transform: rotate(-45deg); transition: transform 0.15s ease; margin: 0 2px; }
details[open] > summary > .chevron { transform: rotate(45deg); }
.command-body { margin: 4px 0 6px; border: 1px solid var(--border-soft); border-radius: 8px; background: var(--bg-panel); overflow: hidden; }
.command-toolbar { display: flex; justify-content: space-between; gap: 8px; flex-wrap: wrap; padding: 10px 14px; color: var(--muted); font-size: 11px; }
.command-entry { border-top: 1px solid var(--border-soft); }
.command-number { color: var(--muted); font: 11px var(--font-mono); font-variant-numeric: tabular-nums; }
.command-line { min-width: 0; flex: 1; color: var(--text-primary); }
.entry-status { display: inline-flex; color: var(--status-idle); flex-shrink: 0; }
@keyframes command-pulse { 50% { opacity: 0.35; } }
@media (prefers-reduced-motion: reduce) {
  .running > .status-dot { animation: none; }
  .chevron, .command-group-header { transition: none; }
}
@media (max-width: 480px) {
  .command-toolbar { padding-left: 10px; padding-right: 10px; }
}
</style>
