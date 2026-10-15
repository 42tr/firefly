<template>
  <span class="conversation-unread-badge" title="回复已完成，尚未阅读" aria-label="回复已完成，尚未阅读">未读</span>
</template>

<style scoped>
.conversation-unread-badge {
  display: inline-flex;
  align-items: center;
  flex: 0 0 auto;
  padding: 1px 5px;
  border-radius: 5px;
  background: rgba(21, 128, 61, 0.08);
  color: var(--success-text);
  font-size: 10px;
  font-weight: 600;
  line-height: 16px;
  white-space: nowrap;
}
</style>
