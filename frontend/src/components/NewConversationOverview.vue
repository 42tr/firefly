<script setup>
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { apiFetch } from '../utils/http'

const content = ref(''), loading = ref(false), error = ref('')
let controller
const generate = async () => {
  controller?.abort()
  const current = controller = new AbortController()
  loading.value = true; error.value = ''
  try {
    const response = await apiFetch('/api/assistant/overview', { method: 'POST', signal: current.signal })
    const data = await response.json().catch(() => null)
    if (!response.ok) throw new Error(data?.error || (response.status === 404 || response.status === 405 ? '概览接口不可用，请更新并重启 Firefly 后端' : '概览暂时无法生成，请稍后重试'))
    if (typeof data?.content !== 'string' || !data.content.trim()) throw new Error('概览返回的数据无效，请稍后重试')
    if (current === controller) content.value = data.content
  } catch (cause) {
    if (current === controller && cause.name !== 'AbortError') error.value = cause.message || '概览暂时无法生成'
  } finally { if (current === controller) loading.value = false }
}
onMounted(generate)
onBeforeUnmount(() => { controller?.abort(); controller = null })
</script>

<template>
  <section class="overview" aria-label="Firefly 概览">
    <span class="eyebrow"><span aria-hidden="true">✳</span> FIREFLY · 你的概览</span>
    <h1>有什么想推进的？</h1>
    <div class="briefing" aria-live="polite" :aria-busy="loading">
      <p v-if="loading && !content" class="state">Firefly 正在整理值得关注的信息…</p>
      <p v-if="content" class="content">{{ content }}</p>
      <p v-if="error" class="state" role="status">{{ error }}</p>
      <button v-if="!loading" type="button" class="refresh" @click="generate">{{ error ? '重试' : '刷新概览' }} <span aria-hidden="true">↗</span></button>
      <span v-else-if="content" class="state">正在更新…</span>
    </div>
  </section>
</template>
<style scoped>
.overview { width: 100%; margin: auto 0; padding: 32px 0; }
.eyebrow { display: inline-flex; align-items: center; gap: 8px; color: var(--muted); font-size: 12px; font-weight: 600; letter-spacing: .06em; }
.eyebrow span { color: #bd8b43; font-size: 17px; }
h1 { margin: 12px 0 24px; color: var(--text-primary); font-size: clamp(24px, 3vw, 32px); font-weight: 600; letter-spacing: -.03em; }
.briefing { padding: 20px 22px; border: 1px solid var(--border-soft); border-radius: 16px; background: var(--bg-panel); }
.content { margin: 0; color: var(--text-primary); white-space: pre-wrap; overflow-wrap: anywhere; font-size: 14px; line-height: 1.9; }
.state { margin: 0; color: var(--muted); font-size: 13px; line-height: 1.7; }
.content + .state { display: block; margin-top: 14px; }
.refresh { margin-top: 16px; padding: 0; border: 0; background: transparent; color: var(--muted); font: inherit; font-size: 12px; cursor: pointer; }.refresh:hover { color: var(--text-primary); }.refresh span { margin-left: 4px; }
@media (max-width: 600px) { .overview { margin: 0; padding: 26px 0; }.briefing { padding: 16px; } }
</style>
