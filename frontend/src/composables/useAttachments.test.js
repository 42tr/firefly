import assert from 'node:assert/strict'
import test from 'node:test'
import { useAttachments } from './useAttachments.js'
import { attachmentContentType } from './attachmentTypes.js'

test('视频可选择、粘贴并预览，移除或清空时释放预览地址', (t) => {
  const revoked = []
  t.mock.method(URL, 'revokeObjectURL', url => revoked.push(url))
  const app = useAttachments()
  const file = new File(['video'], '录屏.MP4', { type: 'application/octet-stream' })
  const target = { files: [file], value: 'selected' }
  app.handleAttachmentChange({ target })
  const entry = app.chatAttachments.value[0]
  assert.equal(entry.type, 'video/mp4')
  assert.match(entry.previewUrl, /^blob:/)
  assert.equal(entry.file, file)
  assert.equal(target.value, '')
  app.removeAttachment(entry.id)
  assert.deepEqual(revoked, [entry.previewUrl])
  const event = pasteEvent('')
  event.clipboardData.items = [{ kind: 'file', getAsFile: () => file }]
  app.handlePaste(event)
  assert.equal(event.defaultPrevented, true)
  assert.equal(app.chatAttachments.value[0].name, '录屏.MP4')
  app.clearAttachments()
  assert.equal(revoked.length, 2)
})

test('前端接受超过原大小上限的普通附件和视频，由服务端校验', (t) => {
  t.mock.method(URL, 'createObjectURL', () => 'blob:video')
  t.mock.method(URL, 'revokeObjectURL', () => {})
  const app = useAttachments()
  const select = (name, size, type = '') => app.handleAttachmentChange({ target: { files: [{ name, size, type }] } })
  select('demo.html', 15 * 1024 * 1024 + 1, 'text/html')
  select('clip.mov', 100 * 1024 * 1024 + 1)
  select('large.txt', 300 * 1024 * 1024)
  assert.equal(app.attachmentError.value, '')
  assert.deepEqual(app.chatAttachments.value.map(({ name }) => name), ['demo.html', 'clip.mov', 'large.txt'])
  assert.equal(attachmentContentType({ name: 'clip.mp4', type: 'text/plain' }), 'text/plain')
  app.clearAttachments()
})

const pasteEvent = (text) => ({
  defaultPrevented: false,
  clipboardData: {
    items: [],
    getData: (type) => (type === 'text/plain' ? text : ''),
  },
  preventDefault() {
    this.defaultPrevented = true
  },
})

test('粘贴超长文本时转为 txt 附件，不进入输入框', async () => {
  const { chatAttachments, handlePaste } = useAttachments()
  const text = '很长的文本'.repeat(500)
  const event = pasteEvent(text)
  handlePaste(event)
  assert.equal(event.defaultPrevented, true)
  assert.equal(chatAttachments.value.length, 1)
  const attachment = chatAttachments.value[0]
  assert.match(attachment.name, /^粘贴文本-\d{8}-\d{6}\.txt$/)
  assert.equal(attachment.type, 'text/plain')
  assert.equal(await attachment.file.text(), text)
})

test('粘贴短文本时保持原样，由输入框接收', () => {
  const { chatAttachments, handlePaste } = useAttachments()
  const event = pasteEvent('短文本')
  handlePaste(event)
  assert.equal(event.defaultPrevented, false)
  assert.equal(chatAttachments.value.length, 0)
})

test('超长付款文本也按普通文本转为附件', () => {
  const { chatAttachments, handlePaste } = useAttachments()
  const event = pasteEvent(`#付款 ${'咖啡 35元 '.repeat(300)}`)
  handlePaste(event)
  assert.equal(event.defaultPrevented, true)
  assert.equal(chatAttachments.value.length, 1)
})

test('粘贴图片时仍走图片附件逻辑', () => {
  const { chatAttachments, handlePaste } = useAttachments()
  const imageFile = new File(['png'], 'image.png', { type: 'image/png' })
  const event = {
    defaultPrevented: false,
    clipboardData: {
      items: [{ kind: 'file', type: 'image/png', getAsFile: () => imageFile }],
      getData: () => '',
    },
    preventDefault() {
      this.defaultPrevented = true
    },
  }
  handlePaste(event)
  assert.equal(event.defaultPrevented, true)
  assert.equal(chatAttachments.value.length, 1)
  assert.match(chatAttachments.value[0].name, /^粘贴图片-\d{8}-\d{6}\.png$/)
})
