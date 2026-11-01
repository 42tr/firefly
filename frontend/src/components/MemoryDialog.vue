<script setup>
import { computed, nextTick, ref, watch, onBeforeUnmount } from 'vue'
import { apiFetch } from '@/utils/http'
import UiIcon from './UiIcon.vue'

const props = defineProps({
  open: { type: Boolean, default: false },
  pendingCount: { type: Number, default: 0 },
})

const emit = defineEmits(['close', 'pending-count'])

const memories = ref([])
const pendingMemories = ref([])
const isLoading = ref(false)
const isSaving = ref(false)
const error = ref('')
const notice = ref('')

// Active memory editor: null = list, -1 = create, otherwise the memory id.
const editingId = ref(null)
const editTitle = ref('')
const editContent = ref('')
const editEnabled = ref(true)
const deleteConfirmId = ref(null)

const pendingEditingId = ref(null)
const pendingEditTitle = ref('')
const pendingEditContent = ref('')
const pendingSavingId = ref(null)

const dialogEl = ref(null)
let previouslyFocused = null

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
    } catch {
      const text = await resp.text().catch(() => '')
      if (text) message = text
    }
    throw new Error(message)
  }
  return resp.json()
}

const normalizePending = (items) => (items || []).map(item => ({ ...item, selected: true }))
const publishPendingCount = () => emit('pending-count', pendingMemories.value.length)

const loadMemories = async () => {
  isLoading.value = true
  error.value = ''
  try {
    const data = await request('/api/assistant/memories')
    memories.value = data.memories || []
    pendingMemories.value = normalizePending(data.pending_memories)
    publishPendingCount()
  } catch (err) {
    error.value = err.message || '读取记忆失败'
  } finally {
    isLoading.value = false
  }
}

const selectedPending = computed(() => pendingMemories.value.filter(item => item.selected))

const startCreate = () => {
  editingId.value = -1
  editTitle.value = ''
  editContent.value = ''
  editEnabled.value = true
  error.value = ''
  notice.value = ''
  deleteConfirmId.value = null
}

const startEdit = (memory) => {
  editingId.value = memory.id
  editTitle.value = memory.title
  editContent.value = memory.content
  editEnabled.value = memory.enabled
  error.value = ''
  notice.value = ''
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
  notice.value = ''
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
    notice.value = '记忆已保存'
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
    memories.value = memories.value.filter(item => item.id !== memory.id)
    if (editingId.value === memory.id) editingId.value = null
  } catch (err) {
    error.value = err.message || '删除记忆失败'
  } finally {
    deleteConfirmId.value = null
  }
}

const startPendingEdit = (candidate) => {
  pendingEditingId.value = candidate.id
  pendingEditTitle.value = candidate.title
  pendingEditContent.value = candidate.content
  error.value = ''
  notice.value = ''
}

const cancelPendingEdit = () => {
  pendingEditingId.value = null
  error.value = ''
}

const savePendingEdit = async (candidate) => {
  const title = pendingEditTitle.value.trim()
  const content = pendingEditContent.value.trim()
  if (!title || !content) {
    error.value = '标题和内容不能为空'
    return
  }
  if ([...title].length + [...content].length > 200) {
    error.value = 'Agent 记忆标题和内容不能超过 200 个字符'
    return
  }
  pendingSavingId.value = candidate.id
  error.value = ''
  try {
    const updated = await request(`/api/assistant/memory-reviews/${candidate.id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title, content }),
    })
    const index = pendingMemories.value.findIndex(item => item.id === candidate.id)
    if (index >= 0) pendingMemories.value[index] = { ...updated, selected: candidate.selected }
    pendingEditingId.value = null
    notice.value = '待确认记忆已更新'
  } catch (err) {
    error.value = err.message || '更新待确认记忆失败'
  } finally {
    pendingSavingId.value = null
  }
}

const confirmPending = async () => {
  const ids = selectedPending.value.map(item => item.id)
  if (!ids.length) {
    error.value = '请至少选择一条待确认记忆'
    return
  }
  isSaving.value = true
  error.value = ''
  notice.value = ''
  try {
    const data = await request('/api/assistant/memory-reviews/confirm', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ids }),
    })
    memories.value = data.memories || []
    pendingMemories.value = normalizePending(data.pending_memories)
    pendingEditingId.value = null
    notice.value = `已保存 ${ids.length} 条记忆`
    publishPendingCount()
  } catch (err) {
    error.value = err.message || '保存待确认记忆失败，请刷新后重试'
  } finally {
    isSaving.value = false
  }
}

const dismissPending = async () => {
  if (!pendingMemories.value.length) return
  isSaving.value = true
  error.value = ''
  notice.value = ''
  try {
    await request('/api/assistant/memory-reviews/dismiss', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ids: [] }),
    })
    pendingMemories.value = []
    pendingEditingId.value = null
    notice.value = '本次未保存任何记忆'
    publishPendingCount()
  } catch (err) {
    error.value = err.message || '忽略待确认记忆失败'
  } finally {
    isSaving.value = false
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
    pendingEditingId.value = null
    deleteConfirmId.value = null
    error.value = ''
    notice.value = ''
    await loadMemories()
    await nextTick()
    dialogEl.value?.focus()
    document.addEventListener('keydown', onKeydown)
  },
  { immediate: true },
)

watch(
  () => props.pendingCount,
  (count, previous) => {
    if (props.open && count !== previous) loadMemories()
  },
)
</script>

<template>
  <div v-if="open" class="dialog-backdrop" @click.self="emit('close')">
    <section
      id="memory-dialog"
      ref="dialogEl"
      class="memory-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="memory-dialog-title"
      tabindex="-1"
    >
      <div class="dialog-header">
        <div>
          <h2 id="memory-dialog-title">长期记忆</h2>
          <p class="memory-subtitle">管理 Agent 在后续对话中可以使用的信息</p>
        </div>
        <button class="dialog-close" type="button" aria-label="关闭" @click="emit('close')"><UiIcon name="x" /></button>
      </div>

      <p class="memory-hint">Agent 建议保存的内容会先放在“待确认”中，只有你确认后才会生效。</p>
      <div v-if="error" class="dialog-error" role="alert">{{ error }}</div>
      <div v-if="notice" class="dialog-notice" role="status">{{ notice }}</div>

      <section class="memory-section pending-section" aria-labelledby="pending-memory-title">
        <div class="section-heading">
          <div><h3 id="pending-memory-title">待确认</h3><p>这些内容还没有写入长期记忆</p></div>
          <span class="pending-count">{{ pendingMemories.length }} 条</span>
        </div>
        <div v-if="isLoading" class="memory-loading" role="status">正在读取记忆…</div>
        <div v-else-if="!pendingMemories.length" class="memory-empty">暂无待确认内容</div>
        <div v-else class="pending-list">
          <article v-for="candidate in pendingMemories" :key="candidate.id" class="pending-item" :class="{ unselected: !candidate.selected }">
            <input v-model="candidate.selected" class="pending-check" type="checkbox" :aria-label="`选择${candidate.title}`" :disabled="isSaving" />
            <div class="pending-main">
              <div v-if="pendingEditingId !== candidate.id">
                <div class="memory-item-header">
                  <span class="pending-badge">{{ candidate.kind === 'update' ? '更新' : '新增' }}</span>
                  <span class="memory-item-title">{{ candidate.title }}</span>
                </div>
                <div class="memory-item-content">{{ candidate.content }}</div>
                <details class="pending-evidence">
                  <summary>查看建议原因</summary>
                  <blockquote>{{ candidate.evidence }}</blockquote>
                </details>
              </div>
              <form v-else class="pending-editor" @submit.prevent="savePendingEdit(candidate)">
                <label>标题<input v-model="pendingEditTitle" type="text" maxlength="100" autocomplete="off" /></label>
                <label>内容<textarea v-model="pendingEditContent" rows="4" maxlength="200"></textarea></label>
                <div class="pending-editor-actions">
                  <button class="button ghost small" type="button" :disabled="pendingSavingId === candidate.id" @click="cancelPendingEdit">取消</button>
                  <button class="button primary small" type="submit" :disabled="pendingSavingId === candidate.id || !pendingEditTitle.trim() || !pendingEditContent.trim()">
                    {{ pendingSavingId === candidate.id ? '保存中…' : '完成编辑' }}
                  </button>
                </div>
              </form>
            </div>
            <div class="pending-actions-inline">
              <button class="button quiet small" type="button" :disabled="isSaving || pendingSavingId === candidate.id" @click="pendingEditingId === candidate.id ? cancelPendingEdit() : startPendingEdit(candidate)">
                {{ pendingEditingId === candidate.id ? '收起' : '编辑' }}
              </button>
            </div>
          </article>
          <div class="pending-footer">
            <span>可以修改内容，也可以只确认其中一条</span>
            <div class="pending-footer-actions">
              <button class="button ghost small" type="button" :disabled="isSaving" @click="dismissPending">暂不保存</button>
              <button class="button primary small" type="button" :disabled="isSaving || !selectedPending.length" @click="confirmPending">
                {{ isSaving ? '保存中…' : `确认保存 ${selectedPending.length} 条` }}
              </button>
            </div>
          </div>
        </div>
      </section>

      <section class="memory-section active-section" aria-labelledby="active-memory-title">
        <div class="section-heading">
          <div><h3 id="active-memory-title">已生效</h3><p>只有这里的内容会参与后续记忆检索</p></div>
          <button class="button primary small" type="button" :disabled="isSaving" @click="startCreate">新增记忆</button>
        </div>

        <form v-if="editingId !== null" class="memory-editor" @submit.prevent="saveMemory">
          <input v-model="editTitle" type="text" maxlength="100" autocomplete="off" placeholder="记忆标题" aria-label="记忆标题" />
          <textarea v-model="editContent" rows="5" maxlength="4000" placeholder="记忆内容" aria-label="记忆内容"></textarea>
          <label class="memory-enabled-toggle"><input v-model="editEnabled" type="checkbox" /> <span>启用（参与对话前的记忆检索）</span></label>
          <div class="dialog-actions">
            <button class="button ghost" type="button" :disabled="isSaving" @click="cancelEdit">取消</button>
            <button class="button primary" type="submit" :disabled="isSaving || !editTitle.trim() || !editContent.trim()">{{ isSaving ? '保存中…' : '保存' }}</button>
          </div>
        </form>

        <div v-if="!isLoading && !memories.length && editingId === null" class="memory-empty">暂无已生效记忆，点击“新增记忆”创建第一条。</div>
        <div v-else-if="editingId === null" class="memory-list">
          <div v-for="memory in memories" :key="memory.id" class="memory-item" :class="{ disabled: !memory.enabled }">
            <div class="memory-item-main">
              <div class="memory-item-header"><span class="memory-item-title">{{ memory.title }}</span><span v-if="!memory.enabled" class="memory-item-badge">已停用</span></div>
              <div class="memory-item-content">{{ memory.content }}</div>
            </div>
            <div class="memory-item-footer">
              <div class="memory-item-details"><span class="memory-item-source" :title="memory.source === 'agent' ? '由 Agent 添加，已由你确认' : '手动添加'">{{ memory.source === 'agent' ? 'Agent 添加' : '手动添加' }}</span><span class="memory-item-meta">更新于 {{ formatUpdatedAt(memory.updated_at) }}</span></div>
              <div class="memory-item-actions">
                <button class="button quiet small" type="button" @click="toggleEnabled(memory)">{{ memory.enabled ? '停用' : '启用' }}</button>
                <button class="button quiet small" type="button" @click="startEdit(memory)">编辑</button>
                <template v-if="deleteConfirmId === memory.id"><button class="button danger small" type="button" @click="deleteMemory(memory)">确认删除</button><button class="button quiet small" type="button" @click="deleteConfirmId = null">取消</button></template>
                <button v-else class="button quiet small" type="button" @click="confirmDelete(memory)">删除</button>
              </div>
            </div>
          </div>
        </div>
      </section>
    </section>
  </div>
</template>

<style scoped>
.dialog-backdrop { position: fixed; inset: 0; z-index: 20; display: grid; place-items: center; padding: 20px; background: rgba(15,20,34,.4); backdrop-filter: blur(6px); -webkit-backdrop-filter: blur(6px); animation: backdropIn .25s ease both; }
.memory-dialog { animation: popIn .28s var(--ease-out); width: min(720px, 100%); max-height: min(820px, calc(100dvh - 40px)); overflow: auto; padding: 22px; background: var(--bg-panel); border: 1px solid var(--border-soft); border-radius: var(--radius-lg); box-shadow: var(--shadow-dialog); outline: none; }
.dialog-header, .dialog-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.dialog-header h2 { margin: 0; font-size: 18px; }
.memory-subtitle { margin: 4px 0 0; color: var(--muted); font-size: 12px; }
.dialog-close { width: 32px; height: 32px; border: 0; background: transparent; color: var(--muted); font-size: 24px; cursor: pointer; }
.memory-hint { margin: 12px 0 0; color: var(--muted); font-size: 12px; line-height: 1.6; }
.dialog-error, .dialog-notice { margin-top: 12px; padding: 9px 11px; border-radius: var(--radius-sm); font-size: 12px; }
.dialog-error { color: var(--danger); background: #fff5f5; }
.dialog-notice { color: var(--success-text); background: #f0faf3; }
.memory-section { margin-top: 22px; }
.pending-section { padding: 14px; border: 1px solid #ebca86; border-radius: var(--radius); background: #fffdf8; }
.active-section { padding-top: 2px; }
.section-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.section-heading h3 { margin: 0; font-size: 14px; }
.section-heading p { margin: 3px 0 0; color: var(--muted); font-size: 11px; }
.pending-count, .pending-badge, .memory-item-badge { padding: 2px 7px; border-radius: 999px; color: #795515; background: #fff4d5; font-size: 11px; white-space: nowrap; }
.memory-loading, .memory-empty { margin-top: 12px; color: var(--muted); font-size: 13px; }
.pending-list { margin-top: 12px; display: flex; flex-direction: column; gap: 9px; }
.pending-item { display: grid; grid-template-columns: 20px minmax(0, 1fr) auto; gap: 10px; align-items: start; padding: 11px 12px; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); background: var(--bg-panel); }
.pending-item.unselected { opacity: .58; background: var(--bg-subtle); }
.pending-check { width: 17px; height: 17px; margin-top: 3px; accent-color: #b67810; }
.pending-main { min-width: 0; }
.memory-item-header { display: flex; align-items: center; flex-wrap: wrap; gap: 7px; }
.memory-item-title { color: var(--text-primary); font-size: 14px; font-weight: 600; overflow-wrap: anywhere; }
.memory-item-content { margin-top: 4px; color: var(--text-secondary); font-size: 12px; line-height: 1.6; white-space: pre-wrap; overflow-wrap: anywhere; }
.pending-evidence { margin-top: 8px; color: var(--muted); font-size: 11px; }
.pending-evidence summary { cursor: pointer; color: #7e6a3e; text-underline-offset: 3px; }
.pending-evidence blockquote { margin: 6px 0 0; padding: 7px 9px; border-radius: 8px; color: var(--text-secondary); background: #faf9f5; }
.pending-actions-inline, .memory-item-actions { display: flex; flex-wrap: wrap; gap: 2px; }
.pending-footer { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 10px; padding-top: 11px; border-top: 1px solid #f0dfb5; color: var(--muted); font-size: 11px; }
.pending-footer-actions { display: flex; gap: 6px; flex-shrink: 0; }
.memory-list { margin-top: 12px; display: flex; flex-direction: column; gap: 10px; }
.memory-item { display: flex; flex-direction: column; gap: 8px; padding: 12px 14px; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); background: var(--bg-body); }
.memory-item.disabled { opacity: .65; }
.memory-item-main { min-width: 0; }
.memory-item-badge { background: var(--border-soft); color: var(--muted); }
.memory-item-footer { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 4px 12px; }
.memory-item-meta { color: var(--muted); font-size: 12px; overflow-wrap: anywhere; }
.memory-item-details { display: flex; align-items: center; flex-wrap: wrap; gap: 4px 8px; min-width: 0; }
.memory-item-source { padding: 2px 6px; border-radius: 4px; background: var(--border-soft); color: var(--text-secondary); font-size: 11px; white-space: nowrap; }
.memory-item-actions .button { padding: 4px 8px; box-shadow: none; }
.memory-editor, .pending-editor { display: flex; flex-direction: column; gap: 10px; }
.memory-editor { margin-top: 14px; }
.memory-editor input[type='text'], .memory-editor textarea, .pending-editor input, .pending-editor textarea { width: 100%; box-sizing: border-box; padding: 8px 12px; border: 1px solid var(--border-soft); border-radius: var(--radius-sm); outline: none; color: var(--text-primary); background: var(--bg-panel); font: inherit; font-size: 13px; line-height: 1.6; }
.memory-editor textarea, .pending-editor textarea { resize: vertical; }
.memory-editor input[type='text']:focus, .memory-editor textarea:focus, .pending-editor input:focus, .pending-editor textarea:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--focus-soft); }
.memory-enabled-toggle { display: flex; align-items: center; gap: 8px; color: var(--text-secondary); font-size: 13px; cursor: pointer; }
.pending-editor label { display: flex; flex-direction: column; gap: 4px; color: var(--muted); font-size: 11px; }
.pending-editor-actions { display: flex; justify-content: flex-end; gap: 7px; }
@media (max-width: 560px) {
  .dialog-close { width: 44px; height: 44px; flex-shrink: 0; border-radius: var(--radius-sm); background: var(--bg-subtle); line-height: 1; }
  .pending-item { grid-template-columns: 20px minmax(0, 1fr); }
  .pending-actions-inline { grid-column: 2; }
  .pending-footer { align-items: stretch; flex-direction: column; }
  .pending-footer-actions { display: grid; grid-template-columns: 1fr 1.35fr; }
  .pending-footer-actions .button { width: 100%; }
  .memory-item-footer { align-items: flex-start; flex-direction: column; }
}
</style>
