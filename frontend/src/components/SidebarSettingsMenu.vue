<script setup>
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import UiIcon from './UiIcon.vue'
import ModulePanel from './ModulePanel.vue'
import AgentSettingsPanel from './AgentSettingsPanel.vue'

const props = defineProps({ active: { type: Boolean, default: true }, collapsed: Boolean })
const emit = defineEmits(['logout'])
const root = ref(null), trigger = ref(null), menu = ref(null), modules = ref(null), agents = ref(null)
const open = ref(false), menuStyle = ref({})
const items = () => [...(menu.value?.querySelectorAll('[role="menuitem"]') || [])]
const contains = target => root.value?.contains(target) || menu.value?.contains(target)
const close = (restoreFocus = false) => {
  open.value = false
  if (restoreFocus) trigger.value?.focus()
}
const restoreFocus = async () => {
  await nextTick()
  await new Promise(resolve => requestAnimationFrame(resolve))
  if (document.querySelector('dialog[open]')) return
  if (props.active && trigger.value?.isConnected) trigger.value.focus()
  else document.querySelector('.mobile-sidebar-toggle')?.focus()
}
const reposition = async () => {
  if (!open.value || !trigger.value) return
  const rect = trigger.value.getBoundingClientRect()
  const width = Math.min(216, window.innerWidth - 16)
  menuStyle.value = {
    left: `${Math.max(8, Math.min(rect.left, window.innerWidth - width - 8))}px`,
    top: '8px', width: `${width}px`, maxHeight: `${window.innerHeight - 16}px`,
  }
  await nextTick()
  if (open.value) menuStyle.value.top = `${Math.max(8, rect.top - (menu.value?.offsetHeight || 0) - 8)}px`
}
const show = async (last = false) => {
  if (!props.active) return
  open.value = true
  await nextTick()
  await reposition()
  if (open.value) (last ? items().at(-1) : items()[0])?.focus()
}
const openModules = () => {
  close(true)
  modules.value?.show()
}
const openAgents = () => {
  close(true)
  agents.value?.show()
}
const logout = () => { close(true); emit('logout') }
const navigate = event => {
  if (event.key === 'Escape') {
    event.preventDefault()
    event.stopPropagation()
    close(true)
  } else if (event.key === 'Tab') close(true)
  else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    event.preventDefault()
    const buttons = items(), index = buttons.indexOf(document.activeElement)
    const target = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1
      : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length
    buttons[target]?.focus()
  }
}
const outside = event => { if (!contains(event.target)) close() }
const focusOut = event => { if (event.relatedTarget && !contains(event.relatedTarget)) close() }
watch(() => props.active, active => { if (!active) close() })
watch(() => props.collapsed, () => close())
onMounted(() => {
  document.addEventListener('pointerdown', outside)
  window.addEventListener('resize', reposition)
  window.addEventListener('scroll', reposition, true)
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', outside)
  window.removeEventListener('resize', reposition)
  window.removeEventListener('scroll', reposition, true)
})
</script>

<template>
  <div ref="root" class="sidebar-settings" @focusout="focusOut">
    <button ref="trigger" class="sidebar-settings-trigger" data-icon-motion="settings" type="button" title="设置" aria-label="设置"
      aria-haspopup="menu" :aria-expanded="open" aria-controls="sidebar-settings-menu"
      @click="open ? close() : show()" @keydown.down.prevent="show()" @keydown.up.prevent="show(true)">
      <UiIcon name="settings" />
    </button>
    <Teleport to="body">
      <div v-if="open" id="sidebar-settings-menu" ref="menu" class="sidebar-settings-menu" :style="menuStyle"
        role="menu" aria-label="设置" @keydown="navigate" @focusout="focusOut">
        <button type="button" role="menuitem" aria-haspopup="dialog" aria-controls="agent-settings-dialog" @click="openAgents"><UiIcon name="sliders" /><span>Agent 配置</span></button>
        <button type="button" role="menuitem" aria-haspopup="dialog" @click="openModules"><UiIcon name="puzzle" /><span>扩展模块</span></button>
        <div class="sidebar-settings-divider" role="separator"></div>
        <button class="sidebar-settings-logout" type="button" role="menuitem" @click="logout"><UiIcon name="logout" /><span>退出登录</span></button>
      </div>
    </Teleport>
    <!-- Dialogs remain available when a layout change hides the navigation drawer. -->
    <ModulePanel ref="modules" active :show-pages="false" :show-trigger="false" @closed="restoreFocus" />
    <AgentSettingsPanel ref="agents" :show-trigger="false" @closed="restoreFocus" />
  </div>
</template>

<style scoped>
.sidebar-settings { position: relative; flex-shrink: 0; margin-top: auto; }
.sidebar-settings-trigger { display: grid; place-items: center; width: 44px; height: 44px; padding: 0; border: 1px solid transparent; border-radius: 14px; background: transparent; color: var(--muted); cursor: pointer; }
.sidebar-settings-trigger .ui-icon { width: 20px; height: 20px; transition: transform 520ms var(--ease-spring); }
.sidebar-settings-trigger[aria-expanded='true'], .sidebar-settings-trigger:focus-visible { color: var(--text-primary); }
.sidebar-settings-trigger[aria-expanded='true'] .ui-icon, .sidebar-settings-trigger:focus-visible .ui-icon { transform: rotate(90deg); }
.sidebar-settings-trigger:active { transform: scale(.94); }
@media (hover: hover) and (pointer: fine) {
  .sidebar-settings-trigger:hover { color: var(--text-primary); }
  .sidebar-settings-trigger:hover .ui-icon { transform: rotate(90deg); }
}
.sidebar-settings-menu { position: fixed; z-index: 200; padding: 6px; overflow-y: auto; overscroll-behavior: contain; border: 1px solid var(--border-soft); border-radius: 16px; background: var(--bg-panel); color: var(--text-primary); box-shadow: var(--shadow-float); transform-origin: 22px bottom; animation: settingsMenuIn 200ms var(--ease-out); }
.sidebar-settings-menu button { display: flex; align-items: center; gap: 12px; width: 100%; min-height: 44px; padding: 8px 12px; border: 0; border-radius: 10px; background: transparent; color: inherit; font: inherit; font-size: 14px; text-align: left; cursor: pointer; transition: background 160ms var(--ease-out); }
.sidebar-settings-menu button:hover, .sidebar-settings-menu button:focus-visible { background: var(--bg-hover); }
.sidebar-settings-menu .ui-icon { width: 18px; height: 18px; flex-shrink: 0; }
.sidebar-settings-divider { height: 1px; margin: 5px 6px; background: var(--border-soft); }
.sidebar-settings-menu .sidebar-settings-logout { color: var(--muted); }
@keyframes settingsMenuIn { from { opacity: 0; transform: translateY(6px) scale(.97); } to { opacity: 1; transform: translateY(0) scale(1); } }
@media (prefers-reduced-motion: reduce) {
  .sidebar-settings-trigger, .sidebar-settings-trigger .ui-icon, .sidebar-settings-menu button { transition: none; }
  .sidebar-settings-trigger:is(:hover, :focus-visible, [aria-expanded='true']) .ui-icon, .sidebar-settings-trigger:active { transform: none; }
  .sidebar-settings-menu { animation: none; }
}
</style>
