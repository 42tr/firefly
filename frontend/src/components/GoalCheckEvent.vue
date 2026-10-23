<script setup>
import UiIcon from './UiIcon.vue'

defineProps({
  result: { type: Object, default: null },
})
</script>

<template>
  <section v-if="result" class="goal-check-event" :class="`goal-check-${result.status || 'needs_review'}`" aria-live="polite">
    <div class="goal-check-icon"><UiIcon :name="result.status === 'completed' ? 'check' : result.status === 'continue' ? 'target' : 'info'" /></div>
    <div class="goal-check-copy">
      <div class="goal-check-title">
        <strong>{{ result.status === 'completed' ? '目标检查：已完成' : result.status === 'continue' ? '目标检查：仍有后续工作' : result.status === 'failed' ? '目标检查失败' : '目标检查：需要确认' }}</strong>
        <span>Goal</span>
      </div>
      <p v-if="result.summary">{{ result.summary }}</p>
      <ul v-if="result.remaining_work?.length">
        <li v-for="item in result.remaining_work" :key="item">{{ item }}</li>
      </ul>
      <p v-if="result.status === 'continue'" class="goal-check-next">将自动继续下一阶段。</p>
    </div>
  </section>
</template>

<style scoped>
.goal-check-event { display: flex; gap: 12px; margin: 4px 0 8px; padding: 16px; border: 1px solid #cfe0ee; border-radius: 16px; background: #f4f9fd; }
.goal-check-completed { border-color: #b7e1c7; background: #f1fbf4; }
.goal-check-needs_review, .goal-check-failed { border-color: #f1d6a7; background: #fffaf0; }
.goal-check-icon { display: grid; place-items: center; width: 32px; height: 32px; flex: 0 0 auto; border-radius: 50%; color: #2671b7; background: #deeffc; }
.goal-check-completed .goal-check-icon { color: var(--success-text); background: #d8f2e0; }
.goal-check-needs_review .goal-check-icon, .goal-check-failed .goal-check-icon { color: var(--warning-text); background: #fff0ce; }
.goal-check-icon .ui-icon { width: 17px; height: 17px; }
.goal-check-copy { min-width: 0; }
.goal-check-title { display: flex; align-items: baseline; gap: 8px; }
.goal-check-title strong { font-size: 14px; }
.goal-check-title span { color: var(--muted); font-size: 11px; }
.goal-check-copy p { margin: 7px 0 0; color: var(--text-secondary); font-size: 13px; line-height: 1.6; }
.goal-check-copy ul { display: grid; gap: 4px; margin: 8px 0 0; padding-left: 18px; color: var(--text-secondary); font-size: 13px; line-height: 1.5; }
.goal-check-next { color: #2671b7 !important; }
</style>
