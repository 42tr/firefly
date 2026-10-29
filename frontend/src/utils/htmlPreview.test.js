import assert from 'node:assert/strict'
import test from 'node:test'
import { HTML_PREVIEW_MAX_BYTES, htmlPreviewErrorMessage, htmlPreviewTooLarge, htmlPreviewUrl, isHtmlPreview } from './htmlPreview.js'

test('html preview accepts only non-directory html files', () => {
  assert.equal(isHtmlPreview({ name: 'index.html' }), true)
  assert.equal(isHtmlPreview({ name: 'index.HTML' }), true)
  assert.equal(isHtmlPreview({ name: 'index.htm' }), true)
  assert.equal(isHtmlPreview({ name: 'docs/Index.Htm' }), true)
  assert.equal(isHtmlPreview({ name: 'index.html.txt' }), false)
  assert.equal(isHtmlPreview({ name: 'index.txt' }), false)
  assert.equal(isHtmlPreview({ name: 'noext' }), false)
  assert.equal(isHtmlPreview({ name: 'folder.html', is_dir: true }), false)
  assert.equal(isHtmlPreview({}), false)
  assert.equal(isHtmlPreview(null), false)
})

test('html preview url encodes the workspace relative path', () => {
  const endpoint = '/api/assistant/conversations/7/files/preview'
  assert.equal(
    htmlPreviewUrl(endpoint, 'design/new chat/原型 #1?.html'),
    `${endpoint}?path=design%2Fnew+chat%2F%E5%8E%9F%E5%9E%8B+%231%3F.html`,
  )
  const url = new URL(htmlPreviewUrl(endpoint, 'a&b/index.html'), 'http://localhost')
  assert.equal(url.pathname, endpoint)
  assert.equal(url.searchParams.get('path'), 'a&b/index.html')
})

test('html preview size guard uses the eight mib boundary', () => {
  assert.equal(HTML_PREVIEW_MAX_BYTES, 8 * 1024 * 1024)
  assert.equal(htmlPreviewTooLarge(HTML_PREVIEW_MAX_BYTES), false)
  assert.equal(htmlPreviewTooLarge(HTML_PREVIEW_MAX_BYTES + 1), true)
  assert.equal(htmlPreviewTooLarge(1024), false)
  // size 缺失时按可预览处理，由 HEAD 预检给出最终判断。
  assert.equal(htmlPreviewTooLarge(undefined), false)
  assert.equal(htmlPreviewTooLarge(null), false)
})

test('html preview errors map to chinese messages', () => {
  assert.match(htmlPreviewErrorMessage(400), /不支持预览/)
  assert.match(htmlPreviewErrorMessage(401), /登录已过期/)
  assert.match(htmlPreviewErrorMessage(404), /文件不存在/)
  assert.match(htmlPreviewErrorMessage(409), /尚未就绪/)
  assert.match(htmlPreviewErrorMessage(413), /8MB/)
  assert.match(htmlPreviewErrorMessage(503), /暂不可用/)
  assert.match(htmlPreviewErrorMessage(500), /预览失败/)
  assert.equal(HTML_PREVIEW_MAX_BYTES, 8 * 1024 * 1024)
})
