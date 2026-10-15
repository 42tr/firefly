<script setup>
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import UiIcon from './UiIcon.vue'
import ModuleApp from './ModuleApp.vue'
import ModuleIndicator from './ModuleIndicator.vue'
import { useModules, moduleRequest, jsonRequest } from '../composables/useModules'
import { formatAppDateTime } from '../composables/appTimezone.js'

const props = defineProps({
  active: Boolean,
  showPages: { type: Boolean, default: true },
  showManagement: { type: Boolean, default: true },
  showTrigger: { type: Boolean, default: true },
})
const emit = defineEmits(['closed', 'page-open'])
const { modules, diagnostics, loading, error, load, save, activeIndicators } = useModules()
const opened = ref(false), dialog = ref(null), selectedId = ref(''), draft = ref({}), enabled = ref(false), jobs = ref([]), busy = ref(false), notice = ref('')
const discovered = ref([]), toolName = ref(''), argumentsText = ref('{}'), toolResult = ref(''), runs = ref([]), activePage = ref(null)
const selected = computed(() => modules.value.find(m => m.id === selectedId.value))
const entries = computed(() => modules.value.filter(m => m.enabled).flatMap(module => (module.pages || []).map(page => ({ module, page }))))
watch(activePage, value => emit('page-open', Boolean(value)))
const fields = computed(() => Object.entries(selected.value?.config_schema?.properties || {}).map(([name, schema]) => ({ name, schema })))
const stateLabels = { disabled: '已停用', ready: '可用', error: '连接异常', running: '执行中' }
const runLabels = { running: '执行中', completed: '完成', failed: '失败', unknown: '结果未知' }
const jsonField = field => ['object', 'array'].includes(field.type)
const choose = module => {
  selectedId.value = module.id; enabled.value = module.enabled; jobs.value = [...module.enabled_jobs]
  draft.value = JSON.parse(JSON.stringify(module.settings))
  if (module.config_schema?.additionalProperties === false) {
    for (const name of Object.keys(draft.value)) {
      if (!(name in (module.config_schema.properties || {}))) delete draft.value[name]
    }
  }
  for (const [name, schema] of Object.entries(module.config_schema?.properties || {})) {
    if (jsonField(schema)) draft.value[name] = draft.value[name] == null ? '' : JSON.stringify(draft.value[name], null, 2)
  }
  notice.value = ''; discovered.value = []; toolName.value = ''; toolResult.value = ''; runs.value = []
}
const close = (force = false) => { if (force === true || !busy.value) { opened.value = false; dialog.value?.close(); emit('closed') } }
const show = async () => { opened.value = true; await nextTick(); dialog.value?.showModal(); await load() }
defineExpose({ show })
const update = async () => {
  busy.value = true; notice.value = ''
  try {
    const settings = { ...draft.value }
    for (const { name, schema } of fields.value) {
      if ((schema.writeOnly || schema.format === 'password') && (settings[name] === '' || settings[name] == null)) {
        if (Object.hasOwn(selected.value.settings, name)) settings[name] = null
        else delete settings[name]
      }
      else if (jsonField(schema)) { if (settings[name]?.trim()) settings[name] = JSON.parse(settings[name]); else delete settings[name] }
      else if (['integer', 'number'].includes(schema.type)) {
        if (settings[name] == null || settings[name] === '') delete settings[name]
        else settings[name] = Number(settings[name])
      }
    }
    await save(selectedId.value, { enabled: enabled.value, settings, enabled_jobs: jobs.value })
    if (props.active && selected.value) { choose(selected.value); notice.value = '已保存' }
  } catch (cause) { notice.value = cause.message }
  finally { busy.value = false }
}
const inspect = async () => {
  busy.value = true; notice.value = ''
  try {
    const data = await moduleRequest(`/${encodeURIComponent(selectedId.value)}/tools`)
    discovered.value = data.tools.filter(tool => !tool._meta?.ui?.visibility || tool._meta.ui.visibility.includes('app'))
    toolName.value = discovered.value[0]?.name || ''
    runs.value = (await moduleRequest(`/${encodeURIComponent(selectedId.value)}/runs`)).runs
  } catch (cause) { notice.value = cause.message }
  finally { busy.value = false }
}
const testTool = async () => {
  busy.value = true; toolResult.value = ''
  try {
    const data = await moduleRequest(`/${encodeURIComponent(selectedId.value)}/call`, jsonRequest('POST', { name: toolName.value, arguments: JSON.parse(argumentsText.value) }))
    toolResult.value = JSON.stringify(data, null, 2)
  } catch (cause) { toolResult.value = cause.message }
  finally { busy.value = false }
}
watch(() => props.active, active => { if (!active) { activePage.value = null; close(true); draft.value = {}; discovered.value = []; toolResult.value = ''; runs.value = [] } })
watch(modules, values => { if (activePage.value && !values.some(module => module.id === activePage.value.module.id && module.enabled)) activePage.value = null })
onBeforeUnmount(() => dialog.value?.close())
</script>

<template>
  <template v-if="showPages">
    <button v-for="entry in entries" :key="entry.module.id + '/' + entry.page.id" class="button ghost small" type="button" role="menuitem" @click="activePage = entry"><UiIcon :name="entry.page.icon || 'file'" /><span>{{ entry.page.title }}</span><ModuleIndicator :indicator="activeIndicators.find(value => value.module_id === entry.module.id)" /></button>
  </template>
  <button v-if="showManagement && showTrigger" class="button ghost small" type="button" role="menuitem" aria-haspopup="dialog" @click="show"><UiIcon name="puzzle" /><span>扩展模块</span></button>
  <ModuleApp v-if="showPages && activePage" :key="activePage.module.id + '/' + activePage.page.id" :module="activePage.module" :page="activePage.page" @close="activePage = null; emit('closed')" />
  <Teleport v-if="showManagement" to="body">
    <dialog ref="dialog" class="modules-dialog" aria-labelledby="modules-title" @keydown.esc.stop @cancel.prevent="close" @click.self="close">
      <header><div><h2 id="modules-title">扩展模块</h2><p>安装自己的工具、页面和自动任务。</p></div><button class="button ghost" :disabled="busy" @click="close">关闭</button></header>
      <div class="modules-body">
        <div class="modules-toolbar"><span>{{ modules.length }} 个已安装模块</span><button class="button ghost small" :disabled="loading || busy" @click="load(true)">重新扫描</button></div>
        <p v-if="loading" role="status">正在读取模块…</p><p v-if="error" role="alert">{{ error }}</p>
        <p v-for="diagnostic in diagnostics" :key="diagnostic.file" role="alert">{{ diagnostic.file }}：{{ diagnostic.error }}</p>
        <p v-if="!loading && !modules.length" class="module-empty">还没有安装模块。将模块包放入服务器的模块目录后，点击“重新扫描”。</p>
        <div class="modules-layout">
          <nav aria-label="已安装模块"><button v-for="module in modules" :key="module.id" :class="['module-card', { selected: selectedId === module.id }]" :disabled="busy" @click="choose(module)"><strong>{{ module.name }}</strong><span>{{ stateLabels[module.status] || module.status }} · {{ module.version }}</span></button></nav>
          <section v-if="selected" class="module-editor" :aria-label="selected.name">
            <h3>{{ selected.name }}</h3><p>{{ selected.description }}</p><p v-if="selected.error" role="alert">{{ selected.error }}</p>
            <label class="check"><input v-model="enabled" type="checkbox" :disabled="busy" />启用模块</label>
            <label v-for="field in fields" :key="field.name" class="module-field">
              <span>{{ field.schema.title || field.name }}{{ selected.config_schema.required?.includes(field.name) ? ' *' : '' }}</span>
              <select v-if="field.schema.enum" v-model="draft[field.name]" :disabled="busy"><option v-for="value in field.schema.enum" :key="String(value)" :value="value">{{ value }}</option></select>
              <input v-else-if="field.schema.type === 'boolean'" v-model="draft[field.name]" type="checkbox" :disabled="busy" />
              <textarea v-else-if="jsonField(field.schema)" v-model="draft[field.name]" rows="4" :disabled="busy"></textarea>
              <input v-else v-model="draft[field.name]" :type="field.schema.writeOnly || field.schema.format === 'password' ? 'password' : ['integer', 'number'].includes(field.schema.type) ? 'number' : 'text'" :placeholder="field.schema.writeOnly || field.schema.format === 'password' ? '留空保留已保存的凭据' : ''" :disabled="busy" autocomplete="off" />
              <small v-if="field.schema.description">{{ field.schema.description }}</small>
            </label>
            <fieldset v-if="selected.jobs.length"><legend>自动任务</legend><label v-for="job in selected.jobs" :key="job.id" class="check"><input v-model="jobs" type="checkbox" :value="job.id" :disabled="busy" />{{ job.id }} · {{ job.cron }} · {{ job.timezone }}</label></fieldset>
            <div class="module-actions"><button class="button primary small" :disabled="busy" @click="update">保存配置</button><button class="button ghost small" :disabled="busy || !selected.enabled" @click="inspect">检查工具与执行记录</button></div>
            <p v-if="notice" role="status">{{ notice }}</p>
            <section v-if="discovered.length" class="module-test" aria-label="工具测试">
              <h4>工具测试</h4><label>工具<select v-model="toolName" :disabled="busy"><option v-for="tool in discovered" :key="tool.name" :value="tool.name">{{ tool.title || tool.name }}</option></select></label>
              <p>{{ discovered.find(t => t.name === toolName)?.description }}</p>
              <details><summary>参数格式</summary><pre>{{ JSON.stringify(discovered.find(t => t.name === toolName)?.inputSchema, null, 2) }}</pre></details>
              <label>参数 JSON<textarea v-model="argumentsText" rows="4" :disabled="busy" spellcheck="false"></textarea></label>
              <button class="button ghost small" :disabled="busy" @click="testTool">执行工具</button><pre v-if="toolResult" role="status">{{ toolResult }}</pre>
            </section>
            <section v-if="runs.length"><h4>最近的自动任务</h4><div v-for="run in runs" :key="run.id" class="module-run"><span>{{ run.job_id }} · {{ runLabels[run.status] }} · {{ formatAppDateTime(run.scheduled_at) }}</span><p v-if="run.error">{{ run.error }}</p><details v-if="run.result"><summary>查看结果</summary><pre>{{ JSON.stringify(run.result, null, 2) }}</pre></details></div></section>
          </section>
        </div>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.modules-dialog { width: min(900px, calc(100vw - 24px)); max-width: none; max-height: 90dvh; padding: 0; border: 1px solid var(--border-soft); border-radius: 14px; color: var(--text-primary); background: var(--bg-panel); }
.modules-dialog::backdrop { background: rgba(15,23,42,.35); }
header { position: sticky; top: 0; z-index: 1; display: flex; justify-content: space-between; align-items: center; padding: 16px; border-bottom: 1px solid var(--border-soft); background: var(--bg-panel); }
h2 { font-size: 18px; margin: 0; } h3 { margin-top: 0; } header p, small, .module-card span { color: var(--text-secondary); font-size: 13px; } header p { margin: 5px 0 0; }
.modules-body { padding: 16px; } .modules-toolbar, .module-actions { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 16px; }
.modules-layout { display: grid; grid-template-columns: minmax(150px, 220px) 1fr; gap: 20px; } nav { display: flex; flex-direction: column; gap: 8px; } .module-card { display: grid; gap: 6px; width: 100%; padding: 12px; text-align: left; color: inherit; background: transparent; border: 1px solid var(--border-soft); border-radius: 10px; cursor: pointer; } .module-card.selected { background: var(--bg-hover); border-color: var(--text-secondary); }
.module-editor { min-width: 0; } .module-field, .module-test label { display: flex; flex-direction: column; gap: 6px; margin: 14px 0; } .module-field input:not([type=checkbox]), textarea, select { box-sizing: border-box; width: 100%; padding: 10px; border: 1px solid var(--border-soft); border-radius: 8px; color: inherit; background: var(--bg-panel); font: inherit; } textarea { resize: vertical; } .check { display: flex; align-items: center; gap: 8px; margin: 10px 0; } fieldset { margin: 16px 0; border: 1px solid var(--border-soft); border-radius: 8px; } .module-actions { justify-content: flex-start; flex-wrap: wrap; margin-top: 16px; } pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 280px; overflow: auto; font-size: 12px; } .module-test, .module-run { border-top: 1px solid var(--border-soft); padding-top: 12px; margin-top: 16px; } .module-empty { line-height: 1.8; }
@media(max-width:600px) { .modules-layout { grid-template-columns: 1fr; } nav { flex-direction: row; overflow-x: auto; } .module-card { min-width: 150px; } }
</style>
