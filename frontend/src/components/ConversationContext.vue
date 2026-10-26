<script setup>
import { onBeforeUnmount, onMounted, ref, useId, watch } from 'vue'

const props = defineProps({
  title: String,
  agent: String,
  agentLabel: String,
  workspace: String,
  workspaceLabel: String,
})
const details = ref(null)
const expanded = ref(false)
const contextId = useId()
const close = (restoreFocus = false) => {
  if (!details.value?.open) return
  details.value.open = false
  if (restoreFocus) details.value.querySelector('summary')?.focus()
}
const outside = event => { if (!details.value?.contains(event.target)) close() }
const resized = () => { if (window.innerWidth > 768) close() }
watch(() => [props.title, props.agentLabel, props.workspaceLabel], () => close())
onMounted(() => {
  document.addEventListener('pointerdown', outside)
  window.addEventListener('resize', resized)
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', outside)
  window.removeEventListener('resize', resized)
})
</script>

<template>
  <div class="toolbar-context">
    <div class="toolbar-context-main">
      <span class="toolbar-title" :title="title">{{ title }}</span>
      <div class="toolbar-metadata" role="group" tabindex="0" aria-label="模型与工作目录">
        <span class="toolbar-agent" :class="agent" :title="agentLabel"><span class="toolbar-agent-label">{{ agentLabel }}</span></span>
        <code v-if="workspace" class="toolbar-workspace" :title="workspaceLabel">{{ workspace }}</code>
      </div>
    </div>
    <details ref="details" class="toolbar-mobile-context" @toggle="expanded = $event.target.open" @keydown.esc.prevent.stop="close(true)" @focusout="event => { if (!details?.contains(event.relatedTarget)) close() }">
      <summary class="toolbar-context-summary" role="button" aria-label="查看对话信息" :aria-expanded="expanded" :aria-controls="contextId">
        <span class="toolbar-context-dot" :class="agent" aria-hidden="true"></span>
        <span class="toolbar-title">{{ title }}</span>
        <svg class="toolbar-context-chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="m4 6 4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </summary>
      <div :id="contextId" class="toolbar-context-card" role="region" aria-label="对话信息" tabindex="0">
        <dl>
          <div><dt>对话</dt><dd>{{ title }}</dd></div>
          <div><dt>模型</dt><dd>{{ agentLabel }}</dd></div>
          <div v-if="workspaceLabel"><dt>工作目录</dt><dd><code>{{ workspaceLabel }}</code></dd></div>
        </dl>
      </div>
    </details>
  </div>
</template>

<style scoped>
.toolbar-context { display: flex; align-items: center; min-width: 0; width: 100%; overflow: hidden; }
.toolbar-context-main { display: flex; align-items: center; gap: 8px; min-width: 0; flex: 0 1 auto; }
.toolbar-title { font-family: var(--font-display); font-weight: 600; font-size: 15px; letter-spacing: -0.01em; max-width: 34vw; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.toolbar-metadata { display: flex; align-items: center; gap: 8px; min-width: 0; }
.toolbar-metadata:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: var(--radius-sm); }
.toolbar-agent { flex: 0 1 auto; min-width: 0; max-width: min(40vw, 320px); overflow: hidden; display: inline-flex; align-items: center; gap: 5px; color: var(--text-secondary); padding: 2px 9px; border-radius: 999px; background: var(--bg-panel); border: 1px solid var(--border-glass); box-shadow: var(--shadow-xs); font-size: 12px; font-weight: 600; line-height: 16px; }
.toolbar-agent::before { content: ''; flex: 0 0 6px; width: 6px; height: 6px; border-radius: 50%; background: var(--glow); box-shadow: 0 0 8px rgba(246, 181, 69, .8); }
.toolbar-agent.firefly::before { background: var(--glow); box-shadow: 0 0 8px rgba(246, 181, 69, .8); }
.toolbar-agent.codex::before { background: #5b8cff; box-shadow: 0 0 8px rgba(91, 140, 255, .8); }
.toolbar-agent.claude::before { background: #e07b53; box-shadow: 0 0 8px rgba(224, 123, 83, .8); }
.toolbar-agent-label { flex: 0 1 auto; min-width: 4ch; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.toolbar-workspace { display: block; min-width: 0; max-width: min(42vw, 420px); padding: 2px 9px; border-radius: 999px; background: var(--bg-subtle); border: 1px solid var(--border-glass); overflow: hidden; color: var(--muted); font-size: 12px; line-height: 16px; text-overflow: ellipsis; white-space: nowrap; }
.toolbar-mobile-context { display: none; }
@media (max-width: 768px) {
  .toolbar-context { overflow: visible; }
  .toolbar-context-main { display: none; }
  .toolbar-title { min-width: 0; max-width: none; line-height: 1.5; }
  .toolbar-mobile-context { display: block; width: 100%; min-width: 0; }
  .toolbar-context-summary {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    min-height: 44px;
    padding: 0 4px;
    border-radius: var(--radius-sm);
    list-style: none;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }
  .toolbar-context-summary::-webkit-details-marker { display: none; }
  .toolbar-context-summary:hover, details[open] .toolbar-context-summary { background: var(--bg-hover); }
  .toolbar-context-summary:focus-visible, .toolbar-context-card:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .toolbar-context-summary .toolbar-title { flex: 1; }
  .toolbar-context-dot { flex: 0 0 6px; width: 6px; height: 6px; border-radius: 50%; background: var(--glow); box-shadow: 0 0 8px rgba(246, 181, 69, .8); }
  .toolbar-context-dot.firefly { background: var(--glow); box-shadow: 0 0 8px rgba(246, 181, 69, .8); }
  .toolbar-context-dot.codex { background: #5b8cff; box-shadow: 0 0 8px rgba(91, 140, 255, .8); }
  .toolbar-context-dot.claude { background: #e07b53; box-shadow: 0 0 8px rgba(224, 123, 83, .8); }
  .toolbar-context-chevron { flex: 0 0 14px; width: 14px; height: 14px; color: var(--muted); transition: transform .15s; }
  details[open] .toolbar-context-chevron { transform: rotate(180deg); }
  .toolbar-context-card {
    position: absolute;
    z-index: 1;
    top: calc(100% + 6px);
    left: 10px;
    right: 10px;
    max-height: min(360px, 60dvh);
    overflow: auto;
    padding: 16px;
    border: 1px solid var(--border-glass);
    border-radius: var(--radius);
    background: var(--bg-panel);
    box-shadow: var(--shadow-float);
  }
  dl { display: grid; gap: 14px; margin: 0; }
  dt { margin-bottom: 4px; color: var(--muted); font-size: 11px; }
  dd { margin: 0; color: var(--text-primary); font-size: 13px; line-height: 1.6; overflow-wrap: anywhere; white-space: pre-wrap; }
  dd code { font-size: 12px; }
}
</style>
