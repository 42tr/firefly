<script setup>
import UiIcon from './UiIcon.vue'
</script>

<template>
  <button class="scroll-bottom-button" type="button" aria-label="回到底部"><UiIcon name="down" /> 回到底部</button>
</template>

<style scoped>
.scroll-bottom-button {
  position: absolute;
  right: 24px;
  bottom: 14px;
  z-index: 10;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 7px 14px;
  border: 1px solid var(--border-glass);
  border-radius: 999px;
  background: var(--bg-glass-strong);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  box-shadow: var(--shadow-float);
  color: var(--accent-strong);
  font: inherit;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  animation: popIn .25s var(--ease-out) both;
  transition: background .2s var(--ease-out), box-shadow .2s var(--ease-out), transform .25s var(--ease-spring);
}
.scroll-bottom-button:hover { background: var(--bg-panel); box-shadow: var(--shadow-glow); transform: translateY(-2px); }
@media (max-width: 768px) { .scroll-bottom-button { right: 12px; bottom: 10px; } }
</style>
