import { ref, onBeforeUnmount } from 'vue'
import { useMarkdown } from '@/composables/useMarkdown'
import { workspaceFileUrl } from './workspaceFileUrl.js'
import { copyText } from '@/utils/clipboard.js'

export function useMessageRendering({ activeConversation, isStreaming, imagePreview }) {
  const { renderMarkdown } = useMarkdown()
  const resolveImageUrl = path => workspaceFileUrl(path, { cwd: activeConversation.value?.cwd })
  const resolveTemporaryUrl = path => {
    let decoded
    try { decoded = decodeURIComponent(path) } catch { return '' }
    return /^(?:sandbox:|file:\/\/(?:localhost)?)?\/tmp\//.test(decoded) || /(?:^|\/)assistant_uploads\//.test(decoded)
      ? resolveImageUrl(path) : ''
  }

  // markdown 渲染缓存：内容未变化时复用上次的 HTML，避免每个 SSE delta
  // 都对所有消息重新 marked.parse + DOMPurify.sanitize
  const markdownCache = new WeakMap()

  // 流式输出期间，正在增长的消息每个 delta 都会全量重算 markdown
  // （marked + DOMPurify + DOMParser 重写链接），长消息下开销呈 O(n²)。
  // 按解析耗时节流，定时器到点后通过 streamRenderTick 触发重渲染。
  const STREAM_RENDER_INTERVAL_MS = 40
  const streamRenderTick = ref(0)
  let streamRenderTimer = null

  const scheduleStreamRender = delay => {
    if (streamRenderTimer) return
    streamRenderTimer = setTimeout(() => {
      streamRenderTimer = null
      streamRenderTick.value += 1
    }, delay)
  }

  const renderCachedHtml = (msg, cache, streaming, cwd, resolveUrl) => {
    // 建立对 streamRenderTick 的依赖：节流定时器到点后重新渲染
    void streamRenderTick.value
    const content = msg.content || ''
    // 缓存 key 除内容外还包含工作目录：切换对话后链接改写结果会变化，
    // 需要让旧缓存失效
    const cached = cache.get(msg)
    const sameContext = cached && cached.cwd === cwd
    if (sameContext && cached.content === content) return cached.html
    // Slow devices/large messages get a larger budget instead of parsing at 60Hz.
    const interval = cached ? Math.min(120, Math.max(STREAM_RENDER_INTERVAL_MS, cached.renderCost * 4)) : STREAM_RENDER_INTERVAL_MS
    const remaining = cached ? interval - (Date.now() - cached.renderedAt) : 0
    if (streaming && sameContext && remaining > 0) {
      scheduleStreamRender(remaining)
      return cached.html
    }
    const startedAt = Date.now()
    const html = enhanceCodeBlocks(rewriteWorkspaceLinks(renderMarkdown(content, { resolveUrl }), cwd))
    const renderedAt = Date.now()
    cache.set(msg, { content, html, renderedAt, renderCost: renderedAt - startedAt, cwd })
    return html
  }

  const renderMessageHtml = msg => renderCachedHtml(msg, markdownCache, isStreaming.value,
    activeConversation.value?.cwd, resolveTemporaryUrl)
  // 代码块包一层带「语言标签 + 复制按钮」的头部（ChatGPT 风格）。
  // 注意在 DOMPurify 消毒之后注入，按钮元素由这里自行创建
  const enhanceCodeBlocks = (html) => {
    if (!html || !html.includes('<pre') || typeof DOMParser === 'undefined') return html
    const doc = new DOMParser().parseFromString(html, 'text/html')
    doc.querySelectorAll('pre').forEach((pre) => {
      const codeClass = pre.querySelector('code')?.getAttribute('class') || ''
      const lang = (codeClass.match(/language-([\w-]+)/) || [])[1] || ''
      const wrapper = doc.createElement('div')
      wrapper.className = 'code-block'
      const header = doc.createElement('div')
      header.className = 'code-block-header'
      const langSpan = doc.createElement('span')
      langSpan.className = 'code-block-lang'
      langSpan.textContent = lang || 'code'
      const copyBtn = doc.createElement('button')
      copyBtn.type = 'button'
      copyBtn.className = 'code-copy-btn'
      copyBtn.textContent = '复制'
      header.append(langSpan, copyBtn)
      pre.replaceWith(wrapper)
      wrapper.append(header, pre)
    })
    return doc.body.innerHTML
  }

  // MarkdownMessage 保留复制按钮节点；用事件委托处理注入 HTML 中的点击。
  const handleChatPanelClick = (event) => {
    const image = event.target?.closest?.('.markdown-body img, .tool-output-image-preview')
    if (image) {
      event.preventDefault()
      const element = image.tagName === 'IMG' ? image : image.querySelector('img')
      if (element) imagePreview.value?.open(element.currentSrc || element.src, element.alt)
      return
    }
    const btn = event.target?.closest?.('.code-copy-btn')
    if (!btn) return
    const text = btn.closest('.code-block')?.querySelector('pre')?.textContent || ''
    if (!text) return
    // Call copyText directly from the click handler so browsers retain the
    // user-activation required by navigator.clipboard.
    copyText(text).then(() => {
      btn.textContent = '已复制'
      setTimeout(() => { btn.textContent = '复制' }, 1500)
    }).catch(() => {
      btn.textContent = '复制失败，请手动选择'
      setTimeout(() => { btn.textContent = '复制' }, 3000)
    })
  }
  const rewriteWorkspaceLinks = (html, cwd) => {
    // 图片和链接都可能指向 Codex 工作区文件，需要统一改写为受保护的下载接口。
    if (!html || (!html.includes('<a ') && !html.includes('<img ')) || typeof DOMParser === 'undefined') return html
    const documentFragment = new DOMParser().parseFromString(html, 'text/html')
    const workspaceUrl = path => workspaceFileUrl(path, { cwd })
    documentFragment.querySelectorAll('img[src]').forEach((image) => {
      const rewritten = workspaceUrl(image.getAttribute('src') || '')
      if (rewritten) image.setAttribute('src', rewritten)
    })
    documentFragment.querySelectorAll('a[href]').forEach((anchor) => {
      const href = anchor.getAttribute('href') || ''
      let url
      try {
        url = new URL(href, window.location.origin)
      } catch (err) {
        return
      }
      const isLocalOrigin = url.origin === window.location.origin || ['localhost', '127.0.0.1'].includes(url.hostname)
      if (!isLocalOrigin) return
      // A model may return an already formed workspace download URL using the
      // repository-relative path (`firefly/workspace/report.html`). Normalize
      // its query through the same path resolver used for Markdown images.
      if (url.pathname === '/api/assistant/workspace/file') {
        const path = url.searchParams.get('path')
        const rewritten = path ? workspaceUrl(path) : ''
        if (rewritten) anchor.setAttribute('href', rewritten)
        return
      }
      if (url.pathname.startsWith('/api/assistant/workspace/')) return
      let pathname
      try {
        pathname = decodeURIComponent(url.pathname)
      } catch (err) {
        return
      }
      if (pathname.startsWith('/tmp/')) {
        anchor.setAttribute('href', workspaceUrl(pathname))
        return
      }
      const marker = /(?:^|\/)workspace(?:\/|$)/
      const markerMatch = pathname.match(marker)
      if (!markerMatch) return
      const relativePath = pathname.slice(markerMatch.index + markerMatch[0].length).replace(/^\/+/, '')
      if (!relativePath || relativePath.endsWith('/')) return
      anchor.setAttribute('href', `/api/assistant/workspace/file?path=${encodeURIComponent(relativePath)}`)
    })
    return documentFragment.body.innerHTML
  }

  onBeforeUnmount(() => { if (streamRenderTimer) clearTimeout(streamRenderTimer) })
  return { renderMessageHtml, handleChatPanelClick, resolveImageUrl }
}
