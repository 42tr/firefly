import { ref } from 'vue'
import { attachmentContentType, isVideoAttachment } from './attachmentTypes.js'
import { appDateParts } from './appTimezone.js'

const MAX_ATTACHMENT_SIZE = 15 * 1024 * 1024
const MAX_VIDEO_ATTACHMENT_SIZE = 100 * 1024 * 1024
// 粘贴超过该长度的纯文本时转为 txt 附件，避免超长文本撑开输入框
const PASTE_TEXT_AS_FILE_MIN_LENGTH = 2000

export function useAttachments() {
  const chatAttachments = ref([])
  const attachmentError = ref('')
  const messageAttachmentUrls = new Set()

  const releaseMessageAttachmentUrls = () => {
    messageAttachmentUrls.forEach((url) => URL.revokeObjectURL(url))
    messageAttachmentUrls.clear()
  }

  const createAttachmentEntry = (file) => {
    const type = attachmentContentType(file)
    const hasPreview = type.startsWith('image/') || type.startsWith('video/')
    return {
      id: `${Date.now()}-${Math.random().toString(16).slice(2)}`,
      file,
      name: file.name || '未命名文件',
      size: file.size || 0,
      type,
      previewUrl: hasPreview ? URL.createObjectURL(file) : '',
    }
  }

  const clearAttachments = (options = { revokePreview: true }) => {
    chatAttachments.value.forEach((item) => {
      if (item.previewUrl && options.revokePreview && !messageAttachmentUrls.has(item.previewUrl)) {
        URL.revokeObjectURL(item.previewUrl)
      }
    })
    chatAttachments.value = []
    attachmentError.value = ''
  }

  const removeAttachment = (id) => {
    const index = chatAttachments.value.findIndex((item) => item.id === id)
    if (index === -1) return
    const [removed] = chatAttachments.value.splice(index, 1)
    if (removed?.previewUrl && !messageAttachmentUrls.has(removed.previewUrl)) {
      URL.revokeObjectURL(removed.previewUrl)
    }
  }

  const addFiles = (files) => {
    if (!files.length) return
    attachmentError.value = ''

    const accepted = files.filter((file) => {
      const limit = isVideoAttachment(file) ? MAX_VIDEO_ATTACHMENT_SIZE : MAX_ATTACHMENT_SIZE
      if (file.size > limit) {
        attachmentError.value = `附件 ${file.name || '未命名文件'} 超过 ${limit / (1024 * 1024)}MB 限制`
        return false
      }
      return true
    })

    const newEntries = accepted.map((file) => createAttachmentEntry(file))
    chatAttachments.value = chatAttachments.value.concat(newEntries)
  }

  const handleAttachmentChange = (event) => {
    const files = Array.from(event?.target?.files || [])
    if (!files.length) return
    addFiles(files)

    if (event?.target) {
      event.target.value = ''
    }
  }

  const formatPasteTimestamp = (date) => {
    const at = appDateParts(date)
    return `${at.year}${at.month}${at.day}-${at.hour}${at.minute}${at.second}`
  }

  const handlePaste = (event) => {
    const items = Array.from(event?.clipboardData?.items || [])
    const fileItems = items.filter((item) => item.kind === 'file')
    const files = fileItems
      .map((item, index) => {
        const file = item.getAsFile()
        if (!file) return null
        if (isVideoAttachment(file)) return file
        if (!file.type?.startsWith('image/')) return null
        if (file.name && file.name !== 'image.png') return file
        const ext = file.type.split('/')[1] || 'png'
        const name = `粘贴图片-${formatPasteTimestamp(new Date())}${fileItems.length > 1 ? `-${index + 1}` : ''}.${ext}`
        return new File([file], name, { type: file.type })
      })
      .filter(Boolean)
    if (files.length) {
      event.preventDefault()
      addFiles(files)
      return
    }

    // 超长文本粘贴转成 txt 附件。
    const text = event?.clipboardData?.getData('text/plain') || ''
    if (text.length < PASTE_TEXT_AS_FILE_MIN_LENGTH) return
    event.preventDefault()
    const name = `粘贴文本-${formatPasteTimestamp(new Date())}.txt`
    addFiles([new File([text], name, { type: 'text/plain' })])
  }

  return {
    chatAttachments,
    attachmentError,
    messageAttachmentUrls,
    releaseMessageAttachmentUrls,
    clearAttachments,
    removeAttachment,
    handleAttachmentChange,
    handlePaste,
  }
}
