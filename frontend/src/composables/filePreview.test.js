import assert from 'node:assert/strict'
import test from 'node:test'
import { filePreviewType, readPreviewBlob } from './filePreview.js'

test('preview treats executable markup as text and excludes unsupported files', () => {
  assert.equal(filePreviewType({ name: 'index.html' }).kind, 'text')
  assert.equal(filePreviewType({ name: 'icon.svg' }).mime, 'text/plain')
  assert.equal(filePreviewType({ name: 'photo.PNG' }).mime, 'image/png')
  assert.equal(filePreviewType({ name: 'archive.zip' }), null)
  assert.equal(filePreviewType({ name: 'folder.txt', is_dir: true }), null)
})

test('preview reads UTF-8 across chunks without losing characters', async () => {
  const bytes = new TextEncoder().encode('中文预览')
  const response = new Response(new ReadableStream({ start(controller) {
    controller.enqueue(bytes.slice(0, 2))
    controller.enqueue(bytes.slice(2))
    controller.close()
  } }))
  const blob = await readPreviewBlob(response, { limit: 100, mime: 'text/plain' })
  assert.equal(await blob.text(), '中文预览')
})

test('oversized preview is cancelled with or without a Content-Length header', async () => {
  for (const headers of [{}, { 'content-length': '100' }]) {
    let cancelled = false
    const response = new Response(new ReadableStream({
      start(controller) { controller.enqueue(new Uint8Array(20)) },
      cancel() { cancelled = true },
    }), { headers })
    await assert.rejects(readPreviewBlob(response, { limit: 10, mime: 'text/plain' }), /超过预览上限/)
    assert.equal(cancelled, true)
  }
})
