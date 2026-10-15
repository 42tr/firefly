const imageTypes = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp', avif: 'image/avif' }
const textExtensions = new Set('txt md markdown json jsonl yaml yml toml ini cfg conf csv tsv log js mjs cjs jsx ts tsx vue svelte css scss less html htm xml svg py rs go java c h cpp hpp cs sh bash zsh sql diff patch'.split(' '))

export function filePreviewType(item) {
  if (!item || item.is_dir) return null
  const name = item.name.toLowerCase()
  const extension = name.split('.').at(-1)
  if (imageTypes[extension]) return { kind: 'image', mime: imageTypes[extension], limit: 10 * 1024 * 1024 }
  if (textExtensions.has(extension) || ['dockerfile', 'makefile', '.gitignore', '.env'].includes(name)) {
    return { kind: 'text', mime: 'text/plain', limit: 1024 * 1024 }
  }
  return null
}

// Enforce the limit while reading too: files may grow after the directory listing.
export async function readPreviewBlob(response, type) {
  const tooLarge = () => new Error(`文件超过预览上限（${type.limit / 1024 / 1024}MB），请下载查看`)
  if (Number(response.headers.get('content-length')) > type.limit) {
    await response.body?.cancel()
    throw tooLarge()
  }
  if (!response.body) throw new Error('无法读取文件内容，请下载查看')
  const reader = response.body.getReader()
  const chunks = []
  let size = 0
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > type.limit) {
        await reader.cancel()
        throw tooLarge()
      }
      chunks.push(value)
    }
  } finally {
    reader.releaseLock()
  }
  return new Blob(chunks, { type: type.mime })
}
