<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import UiIcon from './UiIcon.vue'

const props = defineProps({
  open: Boolean,
  goal: { type: Object, default: null },
  stages: { type: Array, default: () => [] },
  loading: Boolean,
  mutating: Boolean,
  error: { type: String, default: '' },
})

const emit = defineEmits(['close', 'pause', 'resume', 'stop', 'retry'])
const panel = ref(null)
const lastFocus = ref(null)
const active = computed(() => ['planning', 'running', 'checking', 'continuing'].includes(props.goal?.status))
const statusText = computed(() => ({
  planning: '正在准备', running: '自动执行中', checking: '正在检查', continuing: '准备下一阶段',
  paused: '已暂停', completed: '已完成', needs_review: '需要确认', limit_reached: '已达上限', failed: '检查失败', cancelled: '已停止',
}[props.goal?.status] || '未开启'))
const stageText = stage => ({ pending: '待开始', active: '进行中', completed: '已完成', blocked: '受阻' }[stage.status] || '待开始')
const stageClass = stage => `goal-stage-${stage.status || 'pending'}`
const closeOnEscape = event => { if (event.key === 'Escape' && props.open) emit('close') }
const focusPanel = () => panel.value?.querySelector('button, [tabindex="0"]')?.focus()
const trapFocus = event => {
  if (!props.open || event.key !== 'Tab' || !panel.value) return
  const focusables = [...panel.value.querySelectorAll('button:not(:disabled), [tabindex="0"]')]
  if (!focusables.length) return
  const first = focusables[0]
  const last = focusables[focusables.length - 1]
  if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus() }
  else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
}
onMounted(() => { document.addEventListener('keydown', closeOnEscape); document.addEventListener('keydown', trapFocus) })
onBeforeUnmount(() => { document.removeEventListener('keydown', closeOnEscape); document.removeEventListener('keydown', trapFocus) })
const onOpen = () => { lastFocus.value = document.activeElement; requestAnimationFrame(focusPanel) }
const onClose = () => { lastFocus.value?.focus?.(); lastFocus.value = null }
watch(() => props.open, open => { if (open) onOpen(); else onClose() })
</script>

<template>
  <Teleport to="body">
    <Transition name="goal-backdrop">
      <div v-if="open" class="goal-drawer-backdrop" aria-hidden="true" @click="emit('close')"></div>
    </Transition>
    <Transition name="goal-drawer">
      <aside
        v-if="open"
        ref="panel"
        class="goal-drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="goal-drawer-title"
      >
        <header class="goal-drawer-header">
          <div>
            <p class="goal-eyebrow">Goal 模式</p>
            <h2 id="goal-drawer-title">目标进度</h2>
          </div>
          <button class="button icon quiet" type="button" aria-label="关闭目标进度" @click="emit('close')"><UiIcon name="x" /></button>
        </header>

        <div v-if="loading" class="goal-drawer-state" role="status">正在加载目标进度…</div>
        <div v-else-if="!goal" class="goal-drawer-state">
          <UiIcon name="target" />
          <strong>当前没有活动目标</strong>
          <p>打开输入栏中的 Goal 模式，从下一条消息开始持续完成任务。</p>
        </div>
        <template v-else>
          <section class="goal-summary-card">
            <div class="goal-summary-icon"><UiIcon name="target" /></div>
            <div class="goal-summary-copy">
              <strong>{{ goal.prompt }}</strong>
              <span>{{ statusText }}<template v-if="stages.length"> · {{ stages.filter(stage => stage.status === 'completed').length }} / {{ stages.length }} 阶段</template></span>
            </div>
          </section>

          <div v-if="error" class="goal-inline-error" role="alert">{{ error }}</div>

          <section class="goal-timeline" aria-label="目标阶段">
            <div v-for="(stage, index) in stages" :key="stage.id" class="goal-stage" :class="stageClass(stage)">
              <div class="goal-stage-marker" aria-hidden="true">
                <UiIcon v-if="stage.status === 'completed'" name="check" />
                <span v-else-if="stage.status === 'active'" class="goal-stage-pulse"></span>
                <span v-else>{{ index + 1 }}</span>
              </div>
              <div class="goal-stage-line" v-if="index < stages.length - 1" aria-hidden="true"></div>
              <div class="goal-stage-copy">
                <div class="goal-stage-title-row">
                  <strong>{{ index + 1 }}. {{ stage.title }}</strong>
                  <span>{{ stageText(stage) }}</span>
                </div>
                <p v-if="stage.summary">{{ stage.summary }}</p>
              </div>
            </div>
          </section>

          <div v-if="goal.status === 'needs_review' || goal.status === 'failed'" class="goal-review-note" role="alert">
            <UiIcon name="info" />
            <span>目标检查没有得到可靠结论，请重试检查或手动继续。</span>
          </div>

          <footer class="goal-drawer-footer">
            <button v-if="active" class="goal-primary-action" type="button" :disabled="mutating" @click="emit('pause')"><UiIcon name="pause" />暂停自动续接</button>
            <button v-else-if="goal.status === 'paused' || goal.status === 'needs_review' || goal.status === 'failed' || goal.status === 'limit_reached'" class="goal-primary-action" type="button" :disabled="mutating" @click="emit(goal.status === 'failed' || goal.status === 'needs_review' ? 'retry' : 'resume')"><UiIcon name="play" />{{ goal.status === 'failed' || goal.status === 'needs_review' ? '重试检查' : '继续自动续接' }}</button>
            <button v-if="!['completed', 'cancelled'].includes(goal.status)" class="goal-stop-action" type="button" :disabled="mutating" @click="emit('stop')">停止这个目标</button>
            <p>暂停后会完成当前任务，但不会自动开始下一阶段。</p>
          </footer>
        </template>
      </aside>
    </Transition>
  </Teleport>
</template>

<style scoped>
.goal-drawer-backdrop { position: fixed; inset: 0; z-index: 40; background: rgba(23,32,51,.18); backdrop-filter: blur(2px); }
.goal-drawer { position: fixed; inset: 0 0 0 auto; z-index: 41; display: flex; flex-direction: column; width: min(380px, 100vw); padding: 24px 22px 20px; background: var(--bg-panel); border-left: 1px solid var(--border-soft); box-shadow: -12px 0 40px rgba(23,32,51,.12); overflow: auto; }
.goal-drawer-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; margin-bottom: 20px; }
.goal-eyebrow { margin: 0 0 3px; color: var(--glow); font-size: 11px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; }
.goal-drawer h2 { margin: 0; font-size: 22px; letter-spacing: -.03em; }
.goal-summary-card { display: flex; gap: 12px; padding: 15px; border: 1px solid var(--border-soft); border-radius: var(--radius); background: linear-gradient(135deg, #fff 0%, #faf7ef 100%); }
.goal-summary-icon { display: grid; place-items: center; width: 38px; height: 38px; flex: 0 0 auto; border-radius: 12px; color: #8a5a0b; background: var(--glow-soft); }
.goal-summary-icon .ui-icon { width: 20px; height: 20px; }
.goal-summary-copy { display: grid; gap: 6px; min-width: 0; }
.goal-summary-copy strong { color: var(--text-primary); font-size: 14px; line-height: 1.45; overflow-wrap: anywhere; }
.goal-summary-copy span { color: var(--muted); font-size: 12px; }
.goal-timeline { position: relative; padding: 24px 2px 14px; }
.goal-stage { position: relative; display: grid; grid-template-columns: 32px minmax(0, 1fr); gap: 12px; min-height: 92px; }
.goal-stage-marker { position: relative; z-index: 1; display: grid; place-items: center; width: 28px; height: 28px; margin-top: 1px; border: 2px solid #c8d5df; border-radius: 50%; background: var(--bg-panel); color: #8da0b2; font-size: 11px; font-weight: 700; }
.goal-stage-marker .ui-icon { width: 15px; height: 15px; }
.goal-stage-line { position: absolute; top: 27px; bottom: 0; left: 13px; width: 2px; background: #e5ebef; }
.goal-stage.completed .goal-stage-marker, .goal-stage-completed .goal-stage-marker { border-color: #16a05d; background: #16a05d; color: #fff; }
.goal-stage-active .goal-stage-marker { border-color: var(--glow); box-shadow: 0 0 0 7px var(--glow-soft); }
.goal-stage-pulse { width: 9px; height: 9px; border-radius: 50%; background: var(--glow); }
.goal-stage-copy { min-width: 0; padding-bottom: 18px; }
.goal-stage-title-row { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; }
.goal-stage-title-row strong { font-size: 14px; line-height: 1.4; }
.goal-stage-title-row span { flex: 0 0 auto; color: var(--muted); font-size: 11px; }
.goal-stage-completed .goal-stage-title-row span { color: var(--success-text); }
.goal-stage-active .goal-stage-title-row span { color: var(--warning-text); }
.goal-stage-copy p { margin: 7px 0 0; color: var(--text-secondary); font-size: 12px; line-height: 1.55; }
.goal-drawer-footer { display: grid; gap: 10px; margin-top: auto; padding-top: 12px; }
.goal-primary-action, .goal-stop-action { display: inline-flex; align-items: center; justify-content: center; gap: 8px; min-height: 42px; padding: 0 14px; border-radius: var(--radius-sm); font: inherit; font-size: 13px; font-weight: 650; cursor: pointer; }
.goal-primary-action { border: 1px solid #f1c26c; background: #fff7e7; color: #9a630a; }
.goal-primary-action:hover:not(:disabled) { background: #fff0cd; }
.goal-stop-action { border: 0; background: transparent; color: var(--muted); }
.goal-stop-action:hover:not(:disabled) { color: var(--danger); background: var(--bg-subtle); }
.goal-primary-action:disabled, .goal-stop-action:disabled { opacity: .55; cursor: wait; }
.goal-drawer-footer p { margin: 0; color: var(--muted); font-size: 11px; line-height: 1.55; text-align: center; }
.goal-drawer-state { display: grid; justify-items: center; gap: 10px; margin: auto 12px; color: var(--muted); text-align: center; }
.goal-drawer-state .ui-icon { width: 28px; height: 28px; color: var(--glow); }
.goal-drawer-state strong { color: var(--text-primary); font-size: 15px; }
.goal-drawer-state p { margin: 0; font-size: 13px; line-height: 1.6; }
.goal-inline-error, .goal-review-note { display: flex; align-items: flex-start; gap: 8px; margin-top: 12px; padding: 10px 12px; border-radius: var(--radius-sm); background: #fff6ea; color: var(--warning-text); font-size: 12px; line-height: 1.5; }
.goal-inline-error { color: var(--danger); background: #fff1f1; }
.goal-inline-error .ui-icon, .goal-review-note .ui-icon { width: 15px; height: 15px; flex: 0 0 auto; margin-top: 1px; }
.goal-drawer-enter-active, .goal-drawer-leave-active, .goal-backdrop-enter-active, .goal-backdrop-leave-active { transition: opacity .2s ease, transform .24s var(--ease-out); }
.goal-drawer-enter-from, .goal-drawer-leave-to { opacity: 0; transform: translateX(24px); }
.goal-backdrop-enter-from, .goal-backdrop-leave-to { opacity: 0; }
@media (max-width: 768px) { .goal-drawer { width: 100vw; padding: 18px 16px calc(16px + env(safe-area-inset-bottom)); } }
</style>
