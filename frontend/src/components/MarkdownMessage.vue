<script setup>
import { onMounted, ref, watch } from 'vue'

// HTML must come from useMessageRendering (Markdown, sanitization and link rewriting).
const props = defineProps({ html: { type: String, default: '' } })
const body = ref(null)

const compatible = (current, next) => current.nodeType === next.nodeType
  && current.nodeName === next.nodeName && current.namespaceURI === next.namespaceURI

// Keep completed blocks, images and code containers alive. Reconcile the entire
// parsed result so later reference definitions and incomplete Markdown can still
// change earlier blocks; a blank line is not a safe Markdown commit boundary.
const patchChildren = (parent, source) => {
  let current = parent.firstChild
  for (const next of [...source.childNodes]) {
    if (!current) {
      parent.append(next.cloneNode(true))
      continue
    }
    if (current.isEqualNode(next)) {
      current = current.nextSibling
      continue
    }
    // Preserve an existing block when Markdown inserts a node before it.
    if (!compatible(current, next)) {
      if (current.nextSibling?.isEqualNode(next)) {
        const removed = current
        current = current.nextSibling
        removed.remove()
      } else if (next.nextSibling?.isEqualNode(current)) {
        parent.insertBefore(next.cloneNode(true), current)
        continue
      } else {
        const replacement = next.cloneNode(true)
        current.replaceWith(replacement)
        current = replacement.nextSibling
        continue
      }
    }
    if (current.nodeType === Node.ELEMENT_NODE) {
      for (const attribute of [...current.attributes]) {
        if (!next.hasAttribute(attribute.name)) current.removeAttribute(attribute.name)
      }
      for (const attribute of next.attributes) {
        if (current.getAttribute(attribute.name) !== attribute.value) current.setAttribute(attribute.name, attribute.value)
      }
      // Copy feedback belongs to the existing button, not to the stream.
      if (!current.classList.contains('code-copy-btn')) patchChildren(current, next)
    } else if (current.nodeValue !== next.nodeValue) {
      current.nodeValue = next.nodeValue
    }
    current = current.nextSibling
  }
  while (current) {
    const removed = current
    current = current.nextSibling
    removed.remove()
  }
}

const update = () => {
  if (!body.value) return
  const template = document.createElement('template')
  template.innerHTML = props.html
  patchChildren(body.value, template.content)
}
onMounted(update)
watch(() => props.html, update, { flush: 'post' })
</script>

<template>
  <div ref="body" class="markdown-body"></div>
</template>
