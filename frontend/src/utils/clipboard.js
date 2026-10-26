// Copy text while keeping the call in the click event's user-activation
// window. The Clipboard API is unavailable in HTTP and some embedded pages,
// so retain the textarea fallback for those environments.
export function copyText(text) {
  const value = String(text ?? '')
  if (typeof navigator !== 'undefined' && typeof navigator.clipboard?.writeText === 'function') {
    return navigator.clipboard.writeText(value).catch(() => copyWithTextarea(value))
  }

  return copyWithTextarea(value)
}

function copyWithTextarea(value) {
  if (typeof document === 'undefined' || typeof document.execCommand !== 'function') {
    return Promise.reject(new Error('剪贴板不可用'))
  }
  const textarea = document.createElement('textarea')
  textarea.value = value
  textarea.setAttribute('readonly', '')
  textarea.style.position = 'fixed'
  textarea.style.opacity = '0'
  document.body.appendChild(textarea)
  textarea.select()
  try {
    if (!document.execCommand('copy')) throw new Error('复制失败')
    return Promise.resolve()
  } finally {
    textarea.remove()
  }
}
