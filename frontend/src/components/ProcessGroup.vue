<script setup>
import UiIcon from './UiIcon.vue'
import MarkdownMessage from './MarkdownMessage.vue'
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import ToolCallDetails from './ToolCallDetails.vue'
import CommandGroup from './CommandGroup.vue'
import { groupConsecutiveCommands } from '../composables/commandGroups.js'
import { getProcessDurationMs } from '../composables/processDuration.js'

const props = defineProps({
  resolveImageUrl: { type: Function, default: undefined },
  conversationId: Number,
  jobId: String,
  steps: {
    type: Array,
    default: () => [],
  },
  timingSteps: { type: Array, default: null },
  showDuration: { type: Boolean, default: true },
  running: {
    type: Boolean,
    default: false,
  },
  collapseRequested: { type: Boolean, default: false },
  // 过程详情正在按需加载中
  loading: {
    type: Boolean,
    default: false,
  },
  error: { type: String, default: '' },
  toolStatusText: {
    type: Function,
    required: true,
  },
  formatToolValue: {
    type: Function,
    required: true,
  },
  renderMessageHtml: {
    type: Function,
    required: true,
  },
})

const emit = defineEmits(['expand'])
const displaySteps = computed(() => groupConsecutiveCommands(props.steps))

// 默认保持收起；用户手动展开/折叠后以其选择为准
const userChoice = ref(null)
watch(() => props.collapseRequested, requested => {
  if (requested) userChoice.value = false
}, { immediate: true })
const open = computed(() => userChoice.value ?? false)
const toggle = () => {
  userChoice.value = !open.value
  if (userChoice.value) emit('expand')
}

const currentTime = ref(Date.now())
let durationTimer = null
watch(() => props.running, (running) => {
  if (durationTimer) {
    clearInterval(durationTimer)
    durationTimer = null
  }
  currentTime.value = Date.now()
  if (running) durationTimer = setInterval(() => { currentTime.value = Date.now() }, 1000)
}, { immediate: true })
onBeforeUnmount(() => {
  if (durationTimer) clearInterval(durationTimer)
})
const formatCompactDuration = (durationMs) => {
  if (durationMs === null || !Number.isFinite(durationMs)) return ''
  const totalSeconds = Math.max(0, Math.round(durationMs / 1000))
  if (totalSeconds < 60) return `${totalSeconds}s`
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  if (minutes < 60) return `${minutes}m ${seconds}s`
  const hours = Math.floor(minutes / 60)
  const remainingMinutes = minutes % 60
  return remainingMinutes ? `${hours}h ${remainingMinutes}m` : `${hours}h`
}
const durationText = computed(() => props.showDuration ? formatCompactDuration(
  getProcessDurationMs(props.timingSteps || props.steps, props.running, currentTime.value),
) : '')
const summaryText = computed(() => {
  if (!props.showDuration) return '执行过程'
  return durationText.value ? `执行了 ${durationText.value}` : '执行了'
})
</script>

<template>
  <div class="process-group" :class="{ open, running: props.running }">
    <div class="process-group-bar">
      <button
        class="process-group-header"
        type="button"
        :aria-expanded="open"
        :aria-label="open ? '收起执行过程' : '展开执行过程'"
        @click="toggle"
      >
        <span class="process-group-summary">
          <slot name="summary" :duration="durationText">
            <span class="process-group-title">{{ summaryText }}</span>
          </slot>
        </span>
        <span class="process-group-chevron" aria-hidden="true"></span>
      </button>
      <slot name="actions"></slot>
    </div>
    <div v-if="open" class="process-group-body" :aria-busy="loading">
      <div class="process-group-body-heading">
        <span>执行明细</span>
        <span>点击单项查看输入和输出</span>
      </div>
      <div v-if="error" class="process-error" role="alert">{{ error }} <button type="button" class="button ghost small" :disabled="loading" @click="emit('expand')">重试</button></div>
      <div v-if="loading" role="status">正在加载执行详情…</div>
      <template v-for="(step, index) in displaySteps" :key="step.id || `step-${index}`">
        <MarkdownMessage
          v-if="step.role === 'assistant'"
          class="process-step-fragment"
          :html="renderMessageHtml(step)"
        />
        <ToolCallDetails v-else-if="step.kind === 'handoff'" class="handoff-panel" :step="step"
          :conversation-id="conversationId" :job-id="jobId" :format-tool-value="formatToolValue" :resolve-image-url="resolveImageUrl">
          <template #summary><span class="handoff-label">任务移交</span><strong class="handoff-agent">{{ step.agent }}</strong>
            <span class="run-status" :class="step.status">{{ toolStatusText(step.status) }}</span></template>
          <div v-if="step.tools?.length" class="handoff-tools">
            <div class="handoff-tools-label">子 Agent 执行过程</div>
            <ToolCallDetails v-for="toolRun in step.tools" :key="toolRun.id" :step="toolRun"
              :conversation-id="conversationId" :job-id="jobId || step.jobId" :parent-id="step.id"
              :format-tool-value="formatToolValue" :resolve-image-url="resolveImageUrl">
              <template #summary><code :class="toolRun.status">{{ toolRun.name || '工具调用' }}</code>
                <span class="run-status" :class="toolRun.status">{{ toolStatusText(toolRun.status) }}</span></template>
            </ToolCallDetails>
          </div>
        </ToolCallDetails>
        <CommandGroup v-else-if="step.kind === 'command-group'" :commands="step.commands"
          :conversation-id="conversationId" :job-id="jobId" :tool-status-text="toolStatusText"
          :format-tool-value="formatToolValue" :resolve-image-url="resolveImageUrl" />
        <ToolCallDetails v-else-if="step.kind === 'tool-run'" :step="step" :conversation-id="conversationId" :job-id="jobId"
          :format-tool-value="formatToolValue" :resolve-image-url="resolveImageUrl">
          <template #summary><UiIcon class="tool-run-glyph" name="tools" /><code class="tool-name" :class="step.status">{{ step.name || '工具调用' }}</code>
            <span class="run-status" :class="step.status">{{ toolStatusText(step.status) }}</span></template>
        </ToolCallDetails>
        <div v-else-if="step.kind !== 'process-summary'" class="process-step-raw"><code>{{ step.content }}</code></div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.process-error { padding: 10px; color: var(--danger); font-size: 13px; }
.process-group-bar { display: flex; align-items: center; gap: 12px; }
.process-group {
  width: 100%;
  max-width: 100%;
  align-self: flex-start;
  overflow: hidden;
  border: none;
  border-radius: 0;
  background: transparent;
  box-shadow: none;
  animation: fadeSlide 0.4s var(--ease-out) both;
}

/* 收起时只是轻量状态提示，避免与下一条回复之间留下大块空白。 */
.process-group:not(.open) {
  margin-bottom: -16px;
}

.process-group.open {
  border-color: transparent;
  box-shadow: none;
}

.process-group-header {
  display: flex;
  align-items: center;
  gap: 8px;
  width: auto;
  min-height: 32px;
  padding: 3px 2px;
  border: none;
  background: transparent;
  font-family: inherit;
  color: var(--muted);
  cursor: pointer;
  text-align: left;
  transition: color 0.2s var(--ease-out);
}

.process-group-header:hover {
  color: var(--text-primary);
}

.process-group-summary {
  display: flex;
  align-items: center;
  min-width: 0;
}

.process-group-title {
  flex: 0 0 auto;
  color: inherit;
  font-size: 15px;
  font-weight: 400;
  line-height: 1.5;
  white-space: nowrap;
}

/* 运行中的标题带一道流光 */
.process-group.running .process-group-title {
  background: linear-gradient(90deg, var(--muted) 0%, var(--muted) 40%, #d88f14 50%, var(--muted) 60%, var(--muted) 100%);
  background-size: 250% 100%;
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
  animation: shimmer 2.6s linear infinite reverse;
}

.process-group.running .process-group-title::after {
  content: ' ···';
  display: inline-block;
  color: currentColor;
  animation: process-dots 1.15s ease-in-out infinite;
}

@keyframes process-dots {
  0%,
  100% { opacity: 0.25; }
  50% { opacity: 1; }
}

@media (prefers-reduced-motion: reduce) {
  .process-group.running .process-group-title::after { animation: none; opacity: 0.7; }
  .process-group.running .process-group-title { animation: none; color: var(--muted); background: none; }
}

.process-group-chevron {
  width: 8px;
  height: 8px;
  flex: 0 0 auto;
  margin-left: 1px;
  border-right: 1.5px solid currentColor;
  border-bottom: 1.5px solid currentColor;
  opacity: 0;
  transform: rotate(45deg) translate(-2px, 2px);
  transition: transform 0.15s ease, opacity 0.15s ease;
}

.process-group-header:hover .process-group-chevron {
  opacity: 1;
}

@media (min-width: 769px) {
  .process-group-header:focus-visible { color: var(--text-primary); }
  .process-group-header:focus-visible .process-group-chevron { opacity: 1; }
}

.process-group.open .process-group-chevron {
  transform: rotate(225deg) translate(-2px, 2px);
}

.process-group-body {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 5px 0 10px;
  border-top: none;
  background: transparent;
}

.process-group-body-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 0 2px 2px;
  color: var(--muted);
  font-size: 11px;
}

.process-group-body-heading > span:first-child {
  color: var(--text-secondary);
  font-weight: 600;
}

.process-step-fragment {
  padding: 8px 12px;
  border: 1px dashed var(--border-soft);
  border-radius: var(--radius-sm);
  background: var(--bg-subtle);
  color: var(--text-secondary);
  font-size: 13px;
  line-height: 1.65;
}

.process-step-raw {
  padding: 8px 12px;
  border-radius: var(--radius-sm);
  background: var(--bg-subtle);
  border: 1px solid var(--border-soft);
  font-size: 12px;
  color: var(--text-secondary);
  overflow-wrap: anywhere;
}

/* ---------- 工具调用 / 任务移交面板（自 App.vue 迁入，仅在过程块内使用） ---------- */

.handoff-panel {
  overflow: hidden;
  border: 1px solid #e2e2e2;
  border-radius: 10px;
  background: #fff;
  box-shadow: none;
}

.tool-run-glyph {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 25px;
  height: 25px;
  flex: 0 0 auto;
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-sm);
  background: #fff;
  color: var(--status-idle);
  font-family: var(--font-mono);
  font-size: 13px;
  font-weight: 700;
}

.tool-name {
  min-width: 0;
  color: var(--text-primary);
  font-size: 12.5px;
  font-weight: 600;
  overflow-wrap: anywhere;
}

.tool-name.running {
  color: var(--warning-text);
}

.tool-name.completed {
  color: var(--success-text);
}

.tool-name.error {
  color: var(--error-text);
}

.run-status {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  flex: 0 0 auto;
  margin-left: auto;
  color: var(--status-idle);
  font-size: 12px;
  font-weight: 600;
  white-space: nowrap;
}

.run-status.running {
  color: var(--warning-text);
}

.run-status.completed {
  color: var(--success-text);
}

.run-status.error {
  color: var(--error-text);
}

.handoff-tools-label,
.handoff-label {
  color: var(--status-idle);
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.02em;
}

.handoff-panel {
  border-color: var(--accent-border);
  border-left: 3px solid var(--accent);
}

.handoff-agent {
  margin-top: 1px;
  color: var(--accent-strong);
  font-size: 14px;
  font-weight: 600;
}

.handoff-tools {
  padding: 12px 14px 14px;
  border-top: 1px solid var(--accent-border);
  background: var(--bg-subtle);
}

.handoff-tools-label {
  margin-bottom: 8px;
}
</style>
