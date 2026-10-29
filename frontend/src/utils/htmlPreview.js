// HTML 原型预览的纯函数：与 filePreviewType() 解耦，附件文本预览语义保持不变。

export const HTML_PREVIEW_MAX_BYTES = 8 * 1024 * 1024

/** 只有非目录、扩展名为 .html/.htm（大小写不敏感）的文件可以应用内预览。 */
export function isHtmlPreview(item) {
  if (!item || item.is_dir || typeof item.name !== 'string') return false
  const extension = item.name.toLowerCase().split('.').pop()
  return extension === 'html' || extension === 'htm'
}

/** 已知大小即可提前禁用预览，省掉一次 HEAD 往返；size 缺失时交给 HEAD 预检判断。 */
export function htmlPreviewTooLarge(size) {
  return typeof size === 'number' && Number.isFinite(size) && size > HTML_PREVIEW_MAX_BYTES
}

/** 预览 URL：path 必须经 URLSearchParams 编码（空格、中文、#、?、& 都可能出现）。 */
export function htmlPreviewUrl(endpoint, path) {
  return `${endpoint}?${new URLSearchParams({ path })}`
}

/** HEAD 预检的状态码 → 中文提示；错误响应是 JSON，不能直接塞进 iframe。 */
export function htmlPreviewErrorMessage(status) {
  if (status === 401) return '登录已过期，请重新登录'
  if (status === 404) return '文件不存在或已被移除'
  if (status === 409) return '独立工作目录尚未就绪'
  if (status === 413) return '文件超过预览上限（8MB），请下载查看'
  if (status === 503) return '预览服务暂不可用，请重试'
  if (status === 400) return '该文件不支持预览'
  return '预览失败，请重试'
}
