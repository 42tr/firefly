<script setup>
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import UiIcon from './UiIcon.vue'
import ModulePanel from './ModulePanel.vue'

const props = defineProps({
  active: { type: Boolean, default: true },
  schedulesOpen: Boolean,
  filesOpen: Boolean,
  showFiles: Boolean,
  filesDisabled: Boolean,
})
const emit = defineEmits(['open-schedules', 'open-files', 'module-open'])
const open = ref(false), root = ref(null), trigger = ref(null), menu = ref(null)
const items = () => [...(menu.value?.querySelectorAll('[role="menuitem"]') || [])]
const close = (restoreFocus = false) => {
  open.value = false
  if (restoreFocus) trigger.value?.focus()
}
const restoreFocus = async () => {
  await nextTick()
  // Native dialog closure can restore its own focus after Vue's update finishes.
  await new Promise(resolve => requestAnimationFrame(resolve))
  if (props.active && !open.value && trigger.value?.isConnected && !document.querySelector('dialog[open]')) trigger.value.focus()
}
const show = async (last = false) => {
  open.value = true
  await nextTick()
  if (open.value) (last ? items().at(-1) : items()[0])?.focus()
}
const select = event => {
  // Leave the menu before opening a dialog; its close event restores focus below.
  if (event.target.closest('[role="menuitem"]')) close(true)
}
const navigate = event => {
  if (event.key === 'Tab') { close(true); return }
  if (event.key === 'Escape') {
    event.preventDefault()
    event.stopPropagation()
    close(true)
    return
  }
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
  event.preventDefault()
  const buttons = items()
  const index = buttons.indexOf(document.activeElement)
  const target = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1
    : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length
  buttons[target]?.focus()
}
const outside = event => { if (!root.value?.contains(event.target)) close() }
const onFocusOut = event => {
  // Touch browsers can blur the focused item before clicking another button,
  // without giving that button focus. Outside pointer and keyboard events still close the menu.
  if (event.relatedTarget && !root.value?.contains(event.relatedTarget)) close()
}
watch(() => props.active, active => { if (!active) { close() } })
watch(() => props.schedulesOpen, (open, previous) => { if (previous && !open) restoreFocus() })
watch(() => props.filesOpen, (open, previous) => { if (previous && !open) restoreFocus() })
onMounted(() => document.addEventListener('pointerdown', outside))
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', outside)
})
</script>

<template>
  <div ref="root" class="firefly-more" @focusout="onFocusOut">
    <button ref="trigger" class="button ghost icon function-menu-trigger" type="button" aria-label="更多功能" title="功能菜单" aria-haspopup="menu" :aria-expanded="open" aria-controls="firefly-more-menu" @click="open ? close() : show()" @keydown.down.prevent="show()" @keydown.up.prevent="show(true)">
      <UiIcon name="more-horizontal" />
    </button>
    <div v-show="open" id="firefly-more-menu" ref="menu" class="firefly-more-menu" role="menu" aria-label="更多功能" @click="select" @keydown="navigate">
      <ModulePanel :active="props.active" :show-management="false" @closed="restoreFocus" @page-open="emit('module-open', $event)" />
      <button class="button ghost small" type="button" role="menuitem" aria-haspopup="dialog" :aria-expanded="schedulesOpen" aria-controls="schedule-dialog" @click="emit('open-schedules')"><UiIcon name="calendar" /><span>定时任务</span></button>
      <template v-if="showFiles">
        <div class="function-menu-divider" role="separator"></div>
        <button class="button ghost small" type="button" role="menuitem" :disabled="filesDisabled" @click="emit('open-files')"><UiIcon name="folder" /><span>文件</span></button>
      </template>
    </div>
  </div>

</template>

<style scoped>
.firefly-more { position: relative; flex-shrink: 0; }
.function-menu-trigger { position: relative; width: 44px; height: 44px; min-width: 44px; padding: 0; }
.function-menu-trigger .ui-icon { width: 22px; height: 22px; }
.firefly-more-menu { position: absolute; top: calc(100% + 8px); right: 0; z-index: 30; display: flex; flex-direction: column; width: 216px; max-width: calc(100vw - 32px); max-height: calc(100dvh - 88px); overflow-y: auto; overscroll-behavior: contain; padding: 6px; border: 1px solid var(--border-soft); border-radius: 14px; background: var(--bg-panel); box-shadow: 0 10px 28px rgba(23, 32, 51, .1); }
.function-menu-divider { height: 1px; flex-shrink: 0; margin: 5px -6px; background: var(--border-soft); }
.firefly-more-menu :deep(.button.small) { position: relative; display: flex; align-items: center; gap: 12px; width: 100%; min-width: 0; min-height: 40px; justify-content: flex-start; padding: 8px 12px; border: 0; border-radius: 8px; text-align: left; background: transparent; box-shadow: none; transform: none; font-size: 14px; font-weight: 400; color: var(--text-primary); }
.firefly-more-menu :deep(.ui-icon) { display: block; width: 18px; height: 18px; flex-shrink: 0; }
.firefly-more-menu :deep(.button.small:hover:not(:disabled)) { background: var(--bg-hover); box-shadow: none; transform: none; }
@media (min-width: 769px) {
  .firefly-more-menu :deep(.button.small:focus-visible) { background: var(--bg-hover); box-shadow: none; transform: none; }
}
@media (max-width: 768px) {
  .firefly-more-menu { width: 176px; }
  .firefly-more-menu :deep(.button.small) { min-height: 44px; }
}
</style>
