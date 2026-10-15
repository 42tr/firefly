import assert from 'node:assert/strict'
import test from 'node:test'
import { useBrowserNotifications } from './useBrowserNotifications.js'

const replaceGlobal = (t, name, value) => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, name)
  Object.defineProperty(globalThis, name, { configurable: true, writable: true, value })
  t.after(() => {
    if (descriptor) Object.defineProperty(globalThis, name, descriptor)
    else delete globalThis[name]
  })
}

test('notifies for a completed conversation while the page is hidden', (t) => {
  const opened = []
  const notifications = []
  class NotificationMock {
    static permission = 'granted'

    constructor(title, options) {
      this.title = title
      this.options = options
      notifications.push(this)
    }

    close() {
      this.closed = true
    }
  }
  replaceGlobal(t, 'Notification', NotificationMock)
  replaceGlobal(t, 'document', { hidden: true })
  const focus = t.mock.fn()
  replaceGlobal(t, 'window', { focus })

  const { notifyConversationCompleted } = useBrowserNotifications({
    onOpenConversation: (id) => opened.push(id),
  })
  const notification = notifyConversationCompleted({ id: 42, title: '整理周报' })

  assert.equal(notification.title, 'Assistant 对话已完成')
  assert.equal(notification.options.body, '整理周报')
  assert.equal(notification.options.tag, 'assistant-conversation-42')
  notification.onclick()
  assert.equal(focus.mock.callCount(), 1)
  assert.deepEqual(opened, [42])
  assert.equal(notification.closed, true)
})

test('does not notify while the page is visible', (t) => {
  class NotificationMock {
    static permission = 'granted'
  }
  replaceGlobal(t, 'Notification', NotificationMock)
  replaceGlobal(t, 'document', { hidden: false })

  const { notifyConversationCompleted } = useBrowserNotifications()
  assert.equal(notifyConversationCompleted({ id: 1, title: '对话' }), null)
})

test('requests permission only while the decision is pending', async (t) => {
  const requestPermission = t.mock.fn(async () => 'granted')
  class NotificationMock {
    static permission = 'default'
    static requestPermission = requestPermission
  }
  replaceGlobal(t, 'Notification', NotificationMock)

  const { requestPermission: request } = useBrowserNotifications()
  request()
  await Promise.resolve()
  assert.equal(requestPermission.mock.callCount(), 1)

  NotificationMock.permission = 'granted'
  request()
  assert.equal(requestPermission.mock.callCount(), 1)
})
