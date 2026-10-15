// Markdown 和工具结果共用路径解析，确保临时图片以绝对路径读取。
export function workspaceFileUrl(path, { cwd = '' } = {}) {
  if (typeof path !== 'string' || !path) return ''
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
  const marker = '/workspace/'
  const index = decoded.indexOf(marker)
  if (index >= 0) decoded = decoded.slice(index + marker.length)
  else if (!decoded.startsWith('/')) decoded = [String(cwd || '').replace(/^\/+|\/+$/g, ''), decoded.replace(/^\.\//, '')].filter(Boolean).join('/')
  return decoded ? url(decoded) : ''
}
