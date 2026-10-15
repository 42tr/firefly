import DOMPurify from 'dompurify'
import { marked } from 'marked'

marked.setOptions({
  gfm: true,
  breaks: true,
  mangle: false,
  headerIds: false,
})

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'A') {
    node.setAttribute('target', '_blank')
    node.setAttribute('rel', 'noopener noreferrer')
  }
})

// 中文语境下模型常把 ** 紧贴全角标点（如 `。**加粗。**之后`），按 CommonMark 的
// 定界符侧向规则此时 ** 无法开启/闭合强调，marked 会原样输出星号。
// 解析前把这类 **...** 直接转成 <strong>（marked 放行内联 HTML，DOMPurify 允许
// strong）；代码块与行内代码内的 ** 保持原样。
const CODE_PATTERN = /(^|\n)(`{3,}|~{3,})[^\n]*(?:\n[\s\S]*?)?(?:\n\2[ \t]*(?=\n|$)|$)|(`+)([\s\S]*?)\3/g
const STRONG_PATTERN = /(?<![\\*])\*\*(?!\*)(\S(?:(?!\*\*)(?!\n[ \t]*\n)[\s\S])*?)(?<![\s*])\*\*(?!\*)/g

export function fixCjkStrong(content) {
  let result = ''
  let lastIndex = 0
  for (const match of content.matchAll(CODE_PATTERN)) {
    result += content.slice(lastIndex, match.index).replace(STRONG_PATTERN, '<strong>$1</strong>')
    result += match[0]
    lastIndex = match.index + match[0].length
  }
  return result + content.slice(lastIndex).replace(STRONG_PATTERN, '<strong>$1</strong>')
}

export function useMarkdown() {
  function renderMarkdown(content, { resolveUrl } = {}) {
    if (!content) return ''
    const raw = marked.parse(fixCjkStrong(content), {
      async: false,
      // 在消毒前将本地文件协议转换为受保护的 HTTP 地址。
      walkTokens: resolveUrl ? token => {
        if (token.type === 'image' || token.type === 'link') token.href = resolveUrl(token.href) || token.href
      } : undefined,
    })
    return DOMPurify.sanitize(raw, {
      USE_PROFILES: { html: true },
      ADD_TAGS: ['img'],
      ADD_ATTR: ['src', 'alt', 'title', 'loading'],
      ALLOWED_URI_REGEXP:
        /^(?:(?:(?:f|ht)tps?|mailto|tel|callto|cid|xmpp|xxx|data|blob):|[^a-z]|[a-z+.-]+(?:[^a-z+.-:]|$))/i,
    })
  }

  function safeParseJson(raw) {
    if (!raw || typeof raw !== 'string') return null
    try {
      return JSON.parse(raw)
    } catch {
      return null
    }
  }

  return {
    renderMarkdown,
    safeParseJson,
  }
}
