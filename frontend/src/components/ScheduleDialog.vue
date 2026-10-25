<script setup>
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import ChatInputBar from './ChatInputBar.vue'
import NewConversationDialog from './NewConversationDialog.vue'
import { scheduleRequest, useSchedules } from '../composables/useSchedules.js'
import { agentLabel } from '../utils/conversationLabels.js'

const props = defineProps({ open: Boolean })
const emit = defineEmits(['close', 'open-conversation'])
const { schedules, timezone, loading, saving, error, load, mutate } = useSchedules()
const dialog = ref(null), titleInput = ref(null)
const editingId = ref(null), title = ref(''), prompt = ref(''), cron = ref('0 9 * * *'), enabled = ref(true)
const defaultExecution = () => ({ agent: 'firefly', workspace_mode: 'shared', cwd: null, provider: null, model: null, reasoning_level: null })
const execution = ref(defaultExecution()), preset = ref(null), editorKey = ref(0), settingsBusy = ref(false)
const deleteId = ref(null), previewTimes = ref([]), previewError = ref(''), previewLoading = ref(false)
let refreshTimer, previewTimer, previewController, previewRevision = 0
const close = () => { if (!saving.value) emit('close') }
const formatTime = value => value ? new Date(value).toLocaleString('zh-CN', { timeZone: timezone.value, hour12: false }) : '—'
const agentName = agentLabel
const statusName = task => ({ queued: '排队中', running: '执行中', completed: '已完成', failed: '执行失败' }[task.last_status] || (task.last_run_at ? '执行记录已清理' : '尚未执行'))
const canSave = computed(() => title.value.trim() && prompt.value.trim() && cron.value.trim() && !saving.value && !settingsBusy.value && !previewError.value && !previewLoading.value)
const edit = async task => {
  editingId.value = task?.id ?? -1
  title.value = task?.title || ''
  prompt.value = task?.prompt || ''
  cron.value = task?.cron || '0 9 * * *'
  enabled.value = task?.enabled ?? true
  execution.value = task ? { ...task.execution } : defaultExecution()
  preset.value = { ...execution.value }
  settingsBusy.value = false
  editorKey.value += 1
  error.value = ''
  deleteId.value = null
  await nextTick()
  titleInput.value?.focus()
}
const cancelEdit = () => { editingId.value = null; error.value = '' }
const save = async () => {
  if (!canSave.value) return
  const result = await mutate(editingId.value === -1 ? null : editingId.value, editingId.value === -1 ? 'POST' : 'PUT', {
    title: title.value.trim(), prompt: prompt.value.trim(), cron: cron.value.trim(), enabled: enabled.value, execution: execution.value,
  })
  if (result) editingId.value = null
}
const remove = async task => { if (await mutate(task.id, 'DELETE')) deleteId.value = null }
const viewRun = task => { close(); emit('open-conversation', task.last_conversation_id) }
const refresh = () => { if (props.open && editingId.value === null && !document.hidden && !saving.value && !loading.value) load() }

watch(() => props.open, async open => {
  clearInterval(refreshTimer)
  await nextTick()
  if (open && props.open) {
    dialog.value?.showModal()
    load()
    refreshTimer = setInterval(refresh, 5000)
  } else dialog.value?.close()
}, { immediate: true })

watch([cron, editingId, () => props.open], () => {
  const request = ++previewRevision
  clearTimeout(previewTimer)
  previewController?.abort()
  previewTimes.value = []
  previewError.value = ''
  previewLoading.value = false
  if (editingId.value === null || !props.open || !cron.value.trim()) return
  previewLoading.value = true
  previewTimer = setTimeout(async () => {
    previewController = new AbortController()
    try {
      const data = await scheduleRequest('/preview', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ cron: cron.value }), signal: previewController.signal })
      if (request === previewRevision) {
        previewTimes.value = data.times
        timezone.value = data.timezone || timezone.value
      }
    } catch (err) {
      if (request === previewRevision && err.name !== 'AbortError') previewError.value = err.message
    } finally { if (request === previewRevision) previewLoading.value = false }
  }, 300)
})
onBeforeUnmount(() => {
  previewRevision += 1
  clearInterval(refreshTimer)
  clearTimeout(previewTimer)
  previewController?.abort()
  dialog.value?.close()
})
</script>

<template>
  <Teleport to="body">
    <dialog id="schedule-dialog" ref="dialog" class="schedule-dialog" aria-labelledby="schedule-title" @cancel.prevent="close" @click.self="close">
      <header class="schedule-header">
        <div><h2 id="schedule-title">定时任务</h2><p>按 {{ timezone }} 运行，每次执行会新建一条对话。</p></div>
        <button class="button ghost icon" type="button" :disabled="saving" aria-label="关闭定时任务" autofocus @click="close">×</button>
      </header>
      <div class="schedule-body">
        <p v-if="error" class="schedule-error" role="alert">{{ error }}</p>
        <section v-if="editingId !== null" class="schedule-editor" aria-label="定时任务设置">
          <label>任务名称<input ref="titleInput" v-model="title" maxlength="100" :disabled="saving" placeholder="例如：每日项目巡检" /></label>
          <label>cron 表达式<input v-model="cron" class="cron-input" maxlength="120" :disabled="saving" spellcheck="false" placeholder="0 9 * * *" aria-describedby="cron-help" :aria-invalid="!!previewError" /></label>
          <p id="cron-help" class="schedule-hint">分 时 日 月 星期（0 或 7 表示周日），例如 <code>0 9 * * *</code> 表示每天 09:00。</p>
          <div class="cron-presets">
            <button v-for="example in [{ label: '每天 9 点', cron: '0 9 * * *' }, { label: '工作日 9 点', cron: '0 9 * * 1-5' }, { label: '每小时', cron: '0 * * * *' }]" :key="example.cron" class="button quiet small" type="button" :disabled="saving" @click="cron = example.cron">{{ example.label }}</button>
          </div>
          <p v-if="previewError" class="schedule-error" role="alert">{{ previewError }}</p>
          <div v-else class="cron-preview" aria-live="polite">
            <span v-if="previewLoading">正在计算运行时间…</span>
            <template v-else-if="previewTimes.length"><span>接下来三次（{{ timezone }}）</span><ol><li v-for="time in previewTimes" :key="time">{{ formatTime(time) }}</li></ol></template>
          </div>
          <label class="enabled-field"><input v-model="enabled" type="checkbox" :disabled="saving" />启用定时运行</label>
          <div class="prompt-heading"><strong>执行任务</strong><span>Enter 换行 · Ctrl / ⌘ + Enter 保存</span></div>
          <ChatInputBar v-model="prompt" :disabled="saving" :send-disabled="!canSave" :attachments-enabled="false" :submit-on-enter="false" :action-label="saving ? '保存中…' : '保存任务'" input-label="定时任务内容" input-placeholder="输入要定时执行的任务…" @send="save">
            <template #controls><NewConversationDialog :key="editorKey" :open="true" :default-agent="execution.agent" :preset="preset" :creating="saving" @change="execution = $event" @busy="settingsBusy = $event" /></template>
          </ChatInputBar>
          <div class="editor-footer"><button class="button ghost small" type="button" :disabled="saving" @click="cancelEdit">取消</button></div>
        </section>
        <template v-else>
          <div class="schedule-toolbar"><span>{{ schedules.length }} 个任务</span><div><button class="button quiet small" type="button" :disabled="loading || saving" @click="load">刷新</button><button class="button primary small" type="button" :disabled="saving" @click="edit(null)">新增任务</button></div></div>
          <p v-if="loading && !schedules.length" class="schedule-hint" role="status">读取中…</p>
          <p v-else-if="!schedules.length && !error" class="schedule-empty">暂无定时任务，添加任务后会按 cron 自动运行。</p>
          <ul class="schedule-list">
            <li v-for="task in schedules" :key="task.id" class="schedule-card">
              <div class="schedule-card-header"><strong>{{ task.title }}</strong><span class="schedule-badge">{{ agentName(task.execution.agent) }}</span><span v-if="!task.enabled" class="schedule-badge">已停用</span></div>
              <p class="schedule-prompt">{{ task.prompt }}</p>
              <div class="schedule-details"><code>{{ task.cron }}</code><span v-if="task.execution.cwd">{{ task.execution.cwd }}</span><span v-if="task.execution.model">{{ task.execution.model }}</span><span v-if="task.execution.workspace_mode === 'parallel'">独立工作目录</span></div>
              <p class="schedule-time">下次运行：{{ task.enabled ? formatTime(task.next_run_at) : '已停用' }}<span v-if="task.enabled">（{{ timezone }}）</span></p>
              <p class="schedule-time">最近执行：{{ statusName(task) }}<span v-if="task.last_run_at"> · {{ formatTime(task.last_run_at) }}</span></p>
              <p v-if="task.last_notice" class="schedule-hint" role="status">{{ task.last_notice }}</p>
              <p v-if="task.last_error" class="schedule-error">{{ task.last_error }}</p>
              <div class="schedule-actions">
                <button v-if="task.last_conversation_id" class="button quiet small" type="button" :disabled="saving" @click="viewRun(task)">查看执行</button>
                <button class="button quiet small" type="button" :disabled="saving" @click="mutate(task.id, 'PATCH', { enabled: !task.enabled })">{{ task.enabled ? '停用' : '启用' }}</button>
                <button class="button quiet small" type="button" :disabled="saving" @click="edit(task)">编辑</button>
                <template v-if="deleteId === task.id"><button class="button danger small" type="button" :disabled="saving" @click="remove(task)">确认删除</button><button class="button quiet small" type="button" :disabled="saving" @click="deleteId = null">取消</button></template>
                <button v-else class="button quiet small" type="button" :disabled="saving" @click="deleteId = task.id">删除</button>
              </div>
            </li>
          </ul>
          <p class="schedule-hint">上一轮未结束时跳过本次触发；服务离线期间错过的时间合并补跑一次。停用或删除只影响后续触发，已开始的任务可在执行对话中停止。</p>
        </template>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.schedule-dialog { width: min(720px, calc(100vw - 24px)); max-height: min(880px, 90dvh); padding: 0; border: 1px solid var(--border-soft); border-radius: var(--radius-lg); background: var(--bg-panel); color: var(--text-primary); box-shadow: var(--shadow-dialog); overflow-y: auto; overscroll-behavior: contain; }
.schedule-dialog::backdrop { background: rgba(15, 20, 34, .4); backdrop-filter: blur(6px); }
.schedule-header { position: sticky; top: 0; z-index: 2; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 16px 20px; background: var(--bg-panel); border-bottom: 1px solid var(--border-soft); }
.schedule-header h2 { margin: 0; font-size: 18px; }
.schedule-header p { margin: 6px 0 0; color: var(--muted); font-size: 12px; }
.schedule-header button { flex-shrink: 0; width: 44px; height: 44px; }
.schedule-body { padding: 18px 20px; }
.schedule-toolbar, .schedule-card-header, .schedule-actions, .cron-presets, .prompt-heading { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
.schedule-toolbar { justify-content: space-between; margin-bottom: 14px; font-size: 13px; color: var(--muted); }
.schedule-list { list-style: none; padding: 0; margin: 0; display: grid; gap: 12px; }
.schedule-card { padding: 14px; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); background: var(--bg-body); min-width: 0; }
.schedule-card-header strong { font-size: 14px; overflow-wrap: anywhere; }
.schedule-badge { padding: 2px 6px; border-radius: 4px; background: var(--border-soft); color: var(--muted); font-size: 11px; }
.schedule-prompt { white-space: pre-wrap; overflow-wrap: anywhere; color: var(--text-secondary); font-size: 13px; line-height: 1.6; margin: 10px 0; max-height: 160px; overflow-y: auto; }
.schedule-details { display: flex; flex-wrap: wrap; gap: 6px 12px; color: var(--muted); font-size: 12px; overflow-wrap: anywhere; }
.schedule-time, .schedule-hint, .cron-preview { color: var(--muted); font-size: 12px; line-height: 1.7; margin: 6px 0; }
.schedule-actions { margin-top: 8px; gap: 2px; }
.schedule-actions .button { box-shadow: none; }
.schedule-error { color: var(--danger); font-size: 13px; overflow-wrap: anywhere; }
.schedule-empty { padding: 28px 0; color: var(--muted); text-align: center; font-size: 13px; }
.schedule-editor { display: flex; flex-direction: column; gap: 10px; }
.schedule-editor > label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; }
.schedule-editor input:not([type='checkbox']) { box-sizing: border-box; width: 100%; padding: 9px 12px; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); background: var(--bg-body); color: var(--text-primary); font: inherit; }
.schedule-editor .cron-input { font-family: var(--font-mono) !important; }
.schedule-editor > .enabled-field { flex-direction: row; align-items: center; }
.cron-preview { min-height: 24px; padding: 10px 12px; background: var(--bg-body); border-radius: var(--radius-sm); }
.cron-preview ol { margin: 4px 0 0; padding-left: 20px; }
.prompt-heading { justify-content: space-between; margin-top: 12px; font-size: 13px; }
.prompt-heading span { font-size: 12px; color: var(--muted); }
.schedule-editor :deep(.input-bar) { padding: 0; }
.schedule-editor :deep(.chat-input) { min-height: 110px; max-height: 280px; }
.editor-footer { display: flex; justify-content: flex-end; }
@media (max-width: 520px) { .schedule-body { padding: 14px; } .schedule-header { padding: 12px 14px; } .schedule-editor :deep(.send-button.labeled) { padding: 0 8px; } }
</style>
