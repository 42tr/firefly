function stripWorkspacePrefix(value) {
  const match = String(value).match(/(?:^|\/)workspace(?:\/|$)/)
  if (!match) return null
  return String(value).slice(match.index + match[0].length).replace(/^\/+/, '')
}

function normalizeWorkspaceEndpoint(path) {
  const endpoint = '/api/assistant/workspace/file'
  const absolute = /^(?:https?:)/i.test(path)
  if (!absolute && !path.startsWith(endpoint)) return null
  let url
  try {
    url = new URL(path, 'http://workspace.local')
  } catch { return null }
  if (url.pathname !== endpoint) return null
  const relative = stripWorkspacePrefix(url.searchParams.get('path') || '')
  if (relative === null || !relative) return null
  url.searchParams.set('path', relative)
  return absolute ? url.toString() : `${url.pathname}${url.search}${url.hash}`
}

// Markdown 和工具结果共用路径解析，确保临时图片以绝对路径读取。
export function workspaceFileUrl(path, { cwd = '' } = {}) {
  if (typeof path !== 'string' || !path) return ''
  const normalizedEndpoint = normalizeWorkspaceEndpoint(path)
  if (normalizedEndpoint) return normalizedEndpoint
  if (/^(https?:|data:|blob:)/i.test(path) || path.startsWith('/api/')) return path
  let decoded
  try {
    if (path.startsWith('file://')) {
      const url = new URL(path)
      if (url.hostname && url.hostname !== 'localhost') return ''
      decoded = decodeURIComponent(url.pathname)
    } else {
      decoded = decodeURIComponent(path.replace(/^sandbox:/, ''))
    }
  } catch { return '' }
  if (!decoded || decoded.startsWith('//') || /^[a-z][a-z\d+.-]*:/i.test(decoded)) return ''
  // 上传附件由本机保存；UUID 文件没有扩展名。
  const upload = decoded.match(/(?:^|\/)assistant_uploads\/([\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12})$/i)
  if (upload) return `/api/assistant/attachments/by-file/${upload[1]}`
  const url = relative => `/api/assistant/workspace/file?path=${encodeURIComponent(relative)}`
  if (decoded.startsWith('/tmp/')) return url(decoded)
  // Model output may contain either an absolute workspace path or a relative
  // path prefixed with the repository directory (for example
  // `firefly/workspace/dist/report.png`). The API expects a path relative to
  // the configured workspace root.
  const workspaceRelative = stripWorkspacePrefix(decoded)
  if (workspaceRelative !== null) decoded = workspaceRelative
  else if (!decoded.startsWith('/')) {
    let base = String(cwd || '').replace(/^\/+|\/+$/g, '')
    const workspaceBase = stripWorkspacePrefix(base)
    if (workspaceBase !== null) base = workspaceBase
    decoded = [base, decoded.replace(/^\.\//, '')].filter(Boolean).join('/')
  }
  return decoded ? url(decoded) : ''
}
