<script setup>
import { computed } from 'vue'

defineOptions({ name: 'JsonValue' })

const props = defineProps({
  label: {
    type: [String, Number],
    default: null,
  },
  value: {
    type: null,
    default: null,
  },
  depth: {
    type: Number,
    default: 0,
  },
})

const isContainer = computed(() => props.value !== null && typeof props.value === 'object')
const entries = computed(() => (isContainer.value ? Object.entries(props.value) : []))

const valueClass = (value) => {
  if (value === null) return 'null'
  if (Array.isArray(value)) return 'object'
  return typeof value
}

const formatPrimitive = (value) => {
  if (value === null) return 'null'
  if (typeof value === 'string') return JSON.stringify(value)
  return String(value)
}

const formatPreview = (value, depth = 0) => {
  if (value === null || typeof value !== 'object') {
    const primitive = formatPrimitive(value)
    return primitive.length > 56 ? `${primitive.slice(0, 53)}...` : primitive
  }

  const items = Object.entries(value)
  if (depth >= 1) return Array.isArray(value) ? `[${items.length}]` : '{...}'

  const visible = items.slice(0, 3).map(([key, child]) => `${key}: ${formatPreview(child, depth + 1)}`)
  if (items.length > visible.length) visible.push('...')
  return Array.isArray(value) ? `[${visible.join(', ')}]` : `{ ${visible.join(', ')} }`
}

const containerPreview = computed(() => formatPreview(props.value))
</script>

<template>
  <div class="json-value">
    <details v-if="isContainer" :open="depth === 0">
      <summary>
        <span v-if="label !== null" class="json-key">{{ label }}:</span>
        <span class="json-container">{{ containerPreview }}</span>
      </summary>
      <div class="json-children">
        <JsonValue
          v-for="([key, child]) in entries"
          :key="key"
          :label="key"
          :value="child"
          :depth="depth + 1"
        />
      </div>
    </details>
    <div v-else>
      <span v-if="label !== null" class="json-key">{{ label }}:</span>
      <span class="json-primitive" :class="`json-${valueClass(value)}`">{{ formatPrimitive(value) }}</span>
    </div>
  </div>
</template>

<style scoped>
.json-value {
  min-width: 0;
  line-height: 1.6;
}

.json-value summary {
  display: flex;
  align-items: baseline;
  gap: 5px;
  min-height: 20px;
  cursor: pointer;
  list-style: none;
}

.json-value summary::-webkit-details-marker {
  display: none;
}

.json-value summary::before {
  content: '';
  width: 5px;
  height: 5px;
  flex: 0 0 auto;
  margin: 0 2px 2px 1px;
  border-right: 1px solid #64748b;
  border-bottom: 1px solid #64748b;
  transform: rotate(-45deg);
  transition: transform 0.12s ease;
}

.json-value details[open] > summary::before {
  transform: rotate(45deg);
}

.json-children {
  margin-left: 13px;
  padding-left: 9px;
  border-left: 1px solid #dbe3ea;
}

.json-key {
  color: #7c3aed;
}

.json-container {
  color: #64748b;
}

.json-primitive {
  overflow-wrap: anywhere;
}

.json-string {
  color: #047857;
}

.json-number {
  color: #2563eb;
}

.json-boolean {
  color: #b45309;
}

.json-null {
  color: #64748b;
}
</style>
