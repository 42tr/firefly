<script setup>
import { nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { apiFetch } from '@/utils/http'

const props = defineProps({
  open: { type: Boolean, default: false },
})

const emit = defineEmits(['close'])

const memories = ref([])
const isLoading = ref(false)
const isSaving = ref(false)
const error = ref('')

// editingId === null 表示不在编辑；-1 表示新建；其余为记忆 id
const editingId = ref(null)
const editTitle = ref('')
const editContent = ref('')
const editEnabled = ref(true)
const deleteConfirmId = ref(null)
const dialogEl = ref(null)
let previouslyFocused = null

// 对话框没有外部依赖，自己实现焦点管理：打开时把焦点移进对话框（读屏会朗读
// 标题），Tab/Shift+Tab 只在对话框内循环，Esc 关闭，关闭后焦点回到触发元素。
const FOCUSABLE =
  'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'

const onKeydown = (event) => {
  if (event.key === 'Escape') {
    event.preventDefault()
    emit('close')
    return
  }
  if (event.key !== 'Tab') return
  const root = dialogEl.value
  if (!root) return
  const items = Array.from(root.querySelectorAll(FOCUSABLE))
  if (!items.length) {
    event.preventDefault()
    root.focus()
    return
  }
  const first = items[0]
  const last = items[items.length - 1]
  const active = document.activeElement
  const inside = root.contains(active)
  if (event.shiftKey) {
    if (!inside || active === first || active === root) {
      event.preventDefault()
      last.focus()
    }
  } else if (!inside || active === last) {
    event.preventDefault()
    first.focus()
  }
}

const releaseFocus = () => {
  document.removeEventListener('keydown', onKeydown)
  if (previouslyFocused && typeof previouslyFocused.focus === 'function') previouslyFocused.focus()
  previouslyFocused = null
}

onBeforeUnmount(() => document.removeEventListener('keydown', onKeydown))

const request = async (url, options = {}) => {
  const resp = await apiFetch(url, { credentials: 'include', ...options })
  if (!resp.ok) {
    let message = `请求失败 (${resp.status})`
    try {
      const data = await resp.json()
      if (data?.error) message = data.error
    } catch (parseErr) {
      const text = await resp.text().catch(() => '')
      if (text) message = text
    }
    throw new Error(message)
  }
  return resp.json()
}

const loadMemories = async () => {
  isLoading.value = true
  error.value = ''
  try {
    const data = await request('/api/assistant/memories')
    memories.value = data.memories || []
  } catch (err) {
    error.value = err.message || '读取记忆失败'
  } finally {
    isLoading.value = false
  }
}

const startCreate = () => {
  editingId.value = -1
  editTitle.value = ''
  editContent.value = ''
  editEnabled.value = true
  error.value = ''
  deleteConfirmId.value = null
}

const startEdit = (memory) => {
  editingId.value = memory.id
  editTitle.value = memory.title
  editContent.value = memory.content
  editEnabled.value = memory.enabled
  error.value = ''
  deleteConfirmId.value = null
}

const cancelEdit = () => {
  editingId.value = null
  error.value = ''
}

const saveMemory = async () => {
  const title = editTitle.value.trim()
  const content = editContent.value.trim()
  if (!title || !content) {
    error.value = '标题和内容不能为空'
    return
  }
  isSaving.value = true
  error.value = ''
  try {
    if (editingId.value === -1) {
      await request('/api/assistant/memories', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title, content, enabled: editEnabled.value }),
      })
    } else {
      await request(`/api/assistant/memories/${editingId.value}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title, content, enabled: editEnabled.value }),
      })
    }
    editingId.value = null
    await loadMemories()
  } catch (err) {
    error.value = err.message || '保存记忆失败'
  } finally {
    isSaving.value = false
  }
}

const toggleEnabled = async (memory) => {
  error.value = ''
  try {
    await request(`/api/assistant/memories/${memory.id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled: !memory.enabled }),
    })
    memory.enabled = !memory.enabled
  } catch (err) {
    error.value = err.message || '更新状态失败'
  }
}

const confirmDelete = (memory) => {
  deleteConfirmId.value = memory.id
  error.value = ''
}

const deleteMemory = async (memory) => {
  error.value = ''
  try {
    await request(`/api/assistant/memories/${memory.id}`, { method: 'DELETE' })
    memories.value = memories.value.filter((item) => item.id !== memory.id)
    if (editingId.value === memory.id) editingId.value = null
  } catch (err) {
    error.value = err.message || '删除记忆失败'
  } finally {
    deleteConfirmId.value = null
  }
}

const formatUpdatedAt = (value) => {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? '' : date.toLocaleString('zh-CN')
}

watch(
  () => props.open,
  async (open) => {
    if (!open) {
      releaseFocus()
      return
    }
    previouslyFocused = document.activeElement
    editingId.value = null
    deleteConfirmId.value = null
    error.value = ''
    loadMemories()
    await nextTick()
    dialogEl.value?.focus()
    document.addEventListener('keydown', onKeydown)
  },
  { immediate: true },
)
</script>

<template>
  <div v-if="open" class="dialog-backdrop" @click.self="emit('close')">
    <section
      ref="dialogEl"
      class="memory-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="memory-dialog-title"
      tabindex="-1"
    >
      <div class="dialog-header">
        <h2 id="memory-dialog-title">长期记忆</h2>
        <button class="dialog-close" type="button" aria-label="关闭" @click="emit('close')">×</button>
      </div>

      <p class="memory-hint">
        Firefly 会在对话前检索并载入相关记忆。你可以手动添加，Agent 也会自动补充；手动记忆仅由你修改或删除。
      </p>

      <div v-if="error" class="dialog-error">{{ error }}</div>

      <form v-if="editingId !== null" class="memory-editor" @submit.prevent="saveMemory">
        <input
          v-model="editTitle"
          type="text"
          maxlength="100"
          autocomplete="off"
          placeholder="标题，如：更新测试环境部署版本"
          aria-label="记忆标题"
        />
        <textarea
          v-model="editContent"
          rows="6"
          maxlength="4000"
          placeholder="内容，如：调用 POST https://deploy.internal/api/deploy，body 为 {&quot;env&quot;: &quot;test&quot;, &quot;version&quot;: &quot;&lt;版本号&gt;&quot;}，需要 Authorization: Bearer xxx 请求头"
          aria-label="记忆内容"
        ></textarea>
        <label class="memory-enabled-toggle">
          <input v-model="editEnabled" type="checkbox" />
          <span>启用（参与对话前的记忆检索）</span>
        </label>
        <div class="dialog-actions">
          <button class="button ghost" type="button" :disabled="isSaving" @click="cancelEdit">取消</button>
          <button class="button primary" type="submit" :disabled="isSaving || !editTitle.trim() || !editContent.trim()">
            {{ isSaving ? '保存中…' : '保存' }}
          </button>
        </div>
      </form>

      <template v-else>
        <div class="memory-toolbar">
          <button class="button primary small" type="button" @click="startCreate">新增记忆</button>
        </div>
        <div v-if="isLoading" class="memory-loading">读取中…</div>
        <div v-else-if="!memories.length" class="memory-empty">暂无记忆，点击“新增记忆”创建第一条。</div>
        <div v-else class="memory-list">
          <div v-for="memory in memories" :key="memory.id" class="memory-item" :class="{ disabled: !memory.enabled }">
            <div class="memory-item-main">
              <div class="memory-item-header">
                <span class="memory-item-title">{{ memory.title }}</span>
                <span v-if="!memory.enabled" class="memory-item-badge">已停用</span>
              </div>
              <div class="memory-item-content">{{ memory.content }}</div>
            </div>
            <div class="memory-item-footer">
              <div class="memory-item-details">
                <span
                  class="memory-item-source"
                  :title="memory.source === 'agent' ? '由 Agent 自动添加' : '仅你可以修改或删除，Agent 无权操作'"
                >{{ memory.source === 'agent' ? 'Agent 添加' : '手动添加' }}</span>
                <span class="memory-item-meta">更新于 {{ formatUpdatedAt(memory.updated_at) }}</span>
              </div>
              <div class="memory-item-actions">
                <button class="button quiet small" type="button" @click="toggleEnabled(memory)">
                  {{ memory.enabled ? '停用' : '启用' }}
                </button>
                <button class="button quiet small" type="button" @click="startEdit(memory)">编辑</button>
                <template v-if="deleteConfirmId === memory.id">
                  <button class="button danger small" type="button" @click="deleteMemory(memory)">确认删除</button>
                  <button class="button quiet small" type="button" @click="deleteConfirmId = null">取消</button>
                </template>
                <button v-else class="button quiet small" type="button" @click="confirmDelete(memory)">删除</button>
              </div>
            </div>
          </div>
        </div>
      </template>
    </section>
  </div>
</template>

<style scoped>
.dialog-backdrop {
  position: fixed;
  inset: 0;
  z-index: 20;
  display: grid;
  place-items: center;
  padding: 20px;
  background: rgba(15, 20, 34, 0.4);
  backdrop-filter: blur(6px);
  -webkit-backdrop-filter: blur(6px);
  animation: backdropIn 0.25s ease both;
}

.memory-dialog {
  animation: popIn 0.28s var(--ease-out);
  width: min(640px, 100%);
  max-height: min(760px, calc(100dvh - 40px));
  overflow: auto;
  padding: 22px;
  background: var(--bg-panel);
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-dialog);
  /* 打开时容器会获得焦点以便读屏朗读标题，容器本身不可交互，去掉它的焦点环。 */
  outline: none;
}

.dialog-header,
.dialog-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.dialog-header h2 {
  margin: 0;
  font-size: 18px;
}

.dialog-close {
  width: 32px;
  height: 32px;
  border: 0;
  background: transparent;
  color: var(--muted);
  font-size: 24px;
  cursor: pointer;
}

.memory-hint {
  margin: 12px 0 0;
  color: var(--muted);
  font-size: 12px;
  line-height: 1.6;
}

.dialog-error {
  margin-top: 12px;
  color: var(--danger);
  font-size: 12px;
}

.memory-toolbar {
  margin-top: 14px;
  display: flex;
  justify-content: flex-end;
}

.memory-loading,
.memory-empty {
  margin-top: 14px;
  color: var(--muted);
  font-size: 13px;
}

.memory-list {
  margin-top: 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.memory-item {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px 14px;
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-sm);
  background: var(--bg-body);
}

.memory-item.disabled {
  opacity: 0.65;
}

.memory-item-main {
  min-width: 0;
}

.memory-item-header {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}

.memory-item-title {
  color: var(--text-primary);
  font-size: 14px;
  font-weight: 600;
  overflow-wrap: anywhere;
}

.memory-item-badge {
  flex: 0 0 auto;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--border-soft);
  color: var(--muted);
  font-size: 12px;
}

.memory-item-content {
  margin-top: 4px;
  color: var(--text-secondary);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.memory-item-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: 4px 12px;
}

.memory-item-meta {
  color: var(--muted);
  font-size: 12px;
  overflow-wrap: anywhere;
}

.memory-item-details {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 4px 8px;
  min-width: 0;
}

.memory-item-source {
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--border-soft);
  color: var(--text-secondary);
  font-size: 11px;
  white-space: nowrap;
}

.memory-item-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 2px;
}

.memory-item-actions .button {
  padding: 4px 8px;
  box-shadow: none;
}

.memory-editor {
  margin-top: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.memory-editor input[type='text'],
.memory-editor textarea {
  width: 100%;
  box-sizing: border-box;
  padding: 8px 12px;
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-sm);
  outline: none;
  color: var(--text-primary);
  background: var(--bg-panel);
  font: inherit;
  font-size: 13px;
  line-height: 1.6;
}

.memory-editor textarea {
  resize: vertical;
}

.memory-editor input[type='text']:focus,
.memory-editor textarea:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--focus-soft);
}

.memory-enabled-toggle {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--text-secondary);
  font-size: 13px;
  cursor: pointer;
}

.dialog-actions {
  justify-content: flex-end;
}

@media (max-width: 520px) {
  .dialog-close {
    width: 44px;
    height: 44px;
    flex-shrink: 0;
    border-radius: var(--radius-sm);
    background: var(--bg-subtle);
    line-height: 1;
  }

  .memory-item-footer {
    align-items: flex-start;
    flex-direction: column;
  }
}
</style>
