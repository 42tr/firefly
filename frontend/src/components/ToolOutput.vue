<script setup>
import { computed, ref, watch } from 'vue'
import JsonValue from './JsonValue.vue'
import { workspaceFileUrl } from '../composables/workspaceFileUrl.js'

const props = defineProps({
  resolveImageUrl: { type: Function, default: workspaceFileUrl },
  value: {
    type: null,
    default: null,
  },
})

const text = computed(() => {
  if (props.value === null || props.value === undefined) return ''
  if (typeof props.value === 'string') return props.value
  try {
    return JSON.stringify(props.value, null, 2)
  } catch (err) {
    return String(props.value)
  }
})

const jsonValue = computed(() => {
  if (props.value !== null && typeof props.value === 'object') return props.value
  if (typeof props.value !== 'string') return null
  try {
    const parsed = JSON.parse(props.value)
    return parsed !== null && typeof parsed === 'object' ? parsed : null
  } catch (err) {
    return null
  }
})

const imageUrl = computed(() => {
  const value = jsonValue.value
  if (!value || typeof value !== 'object') return ''
  const path = value.image_url || value.imageUrl || value.path
  if (typeof path !== 'string' || !path) return ''
  return props.resolveImageUrl(path)
})

const imageState = ref('loading')
const imageAttempt = ref(0)
watch(imageUrl, () => { imageState.value = 'loading'; imageAttempt.value = 0 })
const retryImage = () => {
  imageState.value = 'loading'
  imageAttempt.value += 1
}

</script>

<template>
  <div class="tool-output">
    <div v-if="imageUrl" class="tool-output-media" :aria-busy="imageState === 'loading'">
      <p v-if="imageState === 'loading'" class="tool-output-image-status" role="status">正在加载图片…</p>
      <div v-if="imageState === 'error'" class="tool-output-image-status" role="alert">
        <span>图片加载失败，文件可能已被清理或暂时无法访问。</span>
        <button type="button" class="tool-output-image-retry" @click="retryImage">重新加载</button>
      </div>
      <button v-show="imageState !== 'error'" type="button" class="tool-output-image-preview"
        :class="{ 'is-loading': imageState === 'loading' }" :disabled="imageState !== 'loaded'" aria-label="放大查看图片">
        <img :key="`${imageUrl}-${imageAttempt}`" class="tool-output-image" :src="imageUrl" alt="工具输出图片" loading="lazy"
          @load="imageState = 'loaded'" @error="imageState = 'error'" />
        <span v-if="imageState === 'loaded'" class="tool-output-image-hint">点击放大</span>
      </button>
    </div>
    <details v-if="imageUrl && jsonValue !== null" class="tool-output-metadata">
      <summary>图片信息</summary>
      <JsonValue :value="jsonValue" />
    </details>
    <JsonValue v-else-if="jsonValue !== null" :value="jsonValue" />
    <pre v-else>{{ text }}</pre>
  </div>
</template>

<style scoped>
.tool-output > .json-value,
.tool-output-metadata > .json-value {
  max-height: 360px;
  margin: 5px 0 0;
  overflow: auto;
  color: #334155;
  font-family: var(--font-mono);
  font-size: 12px;
}

.tool-output pre {
  max-height: 360px;
  margin: 5px 0 0;
  overflow: auto;
  color: #334155;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.55;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.tool-output-image {
  display: block;
  margin: 0 auto;
  max-width: 100%;
  max-height: 520px;
  object-fit: contain;
}

.tool-output-media {
  position: relative;
  min-width: 0;
  overflow: hidden;
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-sm);
  background: var(--bg-subtle);
}
.tool-output-image-preview {
  position: relative;
  display: block;
  width: 100%;
  padding: 0;
  border: 0;
  background: transparent;
  cursor: zoom-in;
}
.tool-output-image-preview.is-loading { min-height: 160px; }
.tool-output-image-preview.is-loading img { opacity: 0; }
.tool-output-image-preview:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
.tool-output-image-status {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  min-height: 160px;
  box-sizing: border-box;
  margin: 0;
  padding: 16px;
  color: var(--text-secondary);
  font-size: 13px;
  text-align: center;
}
p.tool-output-image-status { position: absolute; inset: 0; pointer-events: none; }
.tool-output-image-retry {
  min-height: 44px;
  padding: 8px 16px;
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-sm);
  background: var(--bg-panel);
  color: var(--text-primary);
  font: inherit;
  cursor: pointer;
}
.tool-output-image-hint {
  position: absolute;
  right: 8px;
  bottom: 8px;
  padding: 4px 8px;
  border-radius: 4px;
  background: rgb(0 0 0 / 60%);
  color: #fff;
  font-size: 11px;
}
.tool-output-metadata { margin-top: 8px; color: var(--muted); font-size: 12px; }
.tool-output-metadata summary { cursor: pointer; }

@media (max-width: 600px) {
  .tool-output-image { max-height: 60vh; }
}
</style>
