<script setup>
defineProps({ count: { type: Number, required: true }, summary: Boolean })
</script>

<template>
  <span class="pending-indicator" :class="{ 'pending-indicator--summary': summary }" aria-hidden="true">{{ summary ? `${count} 项待处理` : count > 99 ? '99+' : count }}</span>
</template>

<style scoped>
.pending-indicator { display: inline-flex; align-items: center; justify-content: center; min-width: 18px; height: 18px; padding: 0 4px; border: 1px solid var(--border-soft); border-radius: 5px; background: var(--bg-panel); color: var(--text-secondary); font-size: 10px; font-weight: 500; line-height: 1; font-variant-numeric: tabular-nums; white-space: nowrap; flex-shrink: 0; }
.pending-indicator--summary { justify-content: flex-start; min-width: 0; height: auto; padding: 0; border: 0; border-radius: 0; background: transparent; color: var(--muted); font-size: 11px; font-weight: 400; line-height: 16px; }
</style>
