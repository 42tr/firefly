<script setup>
import { onBeforeUnmount, onMounted, ref, useId } from 'vue'

defineProps({
  messages: { type: Array, default: () => [] },
})

const emit = defineEmits(['remove'])

const expanded = ref(false)
const root = ref(null)
const trigger = ref(null)
const listId = useId()
const close = () => {
  expanded.value = false
  trigger.value?.focus()
}
const onOutsidePointer = event => {
  if (!root.value?.contains(event.target)) expanded.value = false
}
onMounted(() => document.addEventListener('pointerdown', onOutsidePointer))
onBeforeUnmount(() => document.removeEventListener('pointerdown', onOutsidePointer))

const attachmentNames = (message) => {
  const names = message.attachments?.map((attachment) => attachment.name).filter(Boolean) || []
  return names.join('、')
}
</script>

<template>
  <div ref="root" class="input-queue" :class="{ expanded }" @keydown.esc.stop.prevent="close">
    <div class="input-queue-inner">
      <button
        ref="trigger"
        class="input-queue-header"
        type="button"
        :aria-expanded="expanded"
        :aria-controls="listId"
        @click="expanded = !expanded"
      >
        <span class="input-queue-title">
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 4.5h10M3 8h7M3 11.5h5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" /></svg>
          输入队列
        </span>
        <span class="input-queue-summary">{{ messages[0]?.content || '附件消息' }}</span>
        <span class="input-queue-count">{{ messages.length }}</span>
        <svg class="input-queue-chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 10 3.5-3.5 3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
    </div>
    <div v-if="expanded" :id="listId" class="input-queue-list" role="region" aria-label="输入队列">
      <div v-for="(queuedMessage, index) in messages" :key="queuedMessage.id" class="input-queue-item">
        <span class="input-queue-index">{{ index + 1 }}</span>
        <div class="input-queue-content">
          <div v-if="queuedMessage.content" class="input-queue-text">{{ queuedMessage.content }}</div>
          <div v-if="attachmentNames(queuedMessage)" class="input-queue-attachments">
            <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5.5 8.8 9.3 5a2.1 2.1 0 0 1 3 3l-4.9 4.9a3.3 3.3 0 0 1-4.7-4.7l5-5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" /></svg>
            {{ attachmentNames(queuedMessage) }}
          </div>
        </div>
        <button
          class="input-queue-remove"
          type="button"
          :disabled="!queuedMessage.generationJobId"
          :title="queuedMessage.generationJobId ? '从输入队列删除' : '正在提交…'"
          aria-label="从输入队列删除"
          @click="emit('remove', queuedMessage)"
        >
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 4.5 7 7m0-7-7 7" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" /></svg>
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.input-queue {
  flex: 1;
  min-width: 0;
}

.input-queue-inner {
  padding: 9px 10px 4px;
  border: 1px solid var(--border-glass);
  border-radius: var(--radius);
  background: var(--bg-glass-strong);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  box-shadow: var(--shadow-xs);
}

.input-queue-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  gap: 8px;
  border: none;
  background: transparent;
  font: inherit;
  padding: 0 4px 5px;
  color: var(--muted);
  font-size: 12px;
  text-align: left;
  cursor: pointer;
}

.input-queue-title {
  display: inline-flex;
  flex-shrink: 0;
  align-items: center;
  gap: 5px;
  font-weight: 600;
  white-space: nowrap;
}

.input-queue-title svg {
  width: 14px;
  height: 14px;
}

.input-queue-count {
  flex-shrink: 0;
  min-width: 18px;
  padding: 1px 5px;
  border-radius: 999px;
  background: var(--accent-soft);
  color: var(--accent-strong);
  font-size: 12px;
  line-height: 15px;
  text-align: center;
}

.input-queue-summary {
  display: block;
  min-width: 0;
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.input-queue-chevron {
  display: block;
  width: 14px;
  height: 14px;
  flex: 0 0 auto;
  transition: transform 0.2s ease;
}

.input-queue.expanded .input-queue-chevron {
  transform: rotate(180deg);
}

.input-queue-list {
  /* Anchor to the full composer utilities row, including the status column. */
  position: absolute;
  bottom: calc(100% + 6px);
  left: 0;
  right: 0;
  z-index: 10;
  display: flex;
  padding: 6px 10px;
  border: 1px solid var(--border-glass);
  border-radius: var(--radius);
  background: var(--bg-glass-strong);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  box-shadow: var(--shadow-soft);
  overscroll-behavior: contain;
  flex-direction: column;
  max-height: min(180px, 28dvh);
  overflow-y: auto;
}

.input-queue-item {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  padding: 7px 4px;
  border-top: 1px solid var(--border-soft);
}

.input-queue-item:first-child { border-top: 0; }

.input-queue-index {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  flex: 0 0 auto;
  border-radius: 999px;
  background: var(--bg-hover);
  color: var(--muted);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
}

.input-queue-content {
  min-width: 0;
  flex: 1;
}

.input-queue-text,
.input-queue-attachments {
  overflow-wrap: anywhere;
  white-space: pre-wrap;
}

.input-queue-text {
  color: var(--text-primary);
  font-size: 13px;
}

.input-queue-attachments {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  max-width: 100%;
  margin-top: 2px;
  color: var(--muted);
  font-size: 12px;
}

.input-queue-attachments svg {
  width: 13px;
  height: 13px;
  flex: 0 0 auto;
}

.input-queue-remove {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  flex: 0 0 auto;
  padding: 0;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}

.input-queue-remove:hover {
  background: var(--bg-hover);
  color: var(--danger);
}

.input-queue-remove:disabled {
  color: var(--accent-border);
  cursor: wait;
}

.input-queue-remove svg {
  width: 15px;
  height: 15px;
}

@media (pointer: coarse) {
  .input-queue-remove { min-height: 44px; min-width: 44px; }
}

@media (max-width: 768px) {
  .input-queue-inner {
    padding: 3px 8px 2px;
    border-radius: var(--radius-sm);
  }

  .input-queue-header {
    min-height: 30px;
    padding: 0 2px;
  }

  .input-queue-item {
    padding-top: 4px;
    padding-bottom: 4px;
  }

  .input-queue-remove {
    width: 32px;
    min-width: 32px;
    height: 32px;
    min-height: 32px;
  }
}
</style>
