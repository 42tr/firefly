const NOTIFICATION_ICON = '/assistant-icon.svg'

export function useBrowserNotifications({ onOpenConversation } = {}) {
  const notificationApi = globalThis.Notification

  const requestPermission = () => {
    if (!notificationApi || notificationApi.permission !== 'default') return
    try {
      Promise.resolve(notificationApi.requestPermission()).catch((err) => {
        console.error('Failed to request notification permission', err)
      })
    } catch (err) {
      console.error('Failed to request notification permission', err)
    }
  }

  const notifyConversationCompleted = (conversation) => {
    if (!notificationApi || notificationApi.permission !== 'granted' || !globalThis.document?.hidden) return null

    const title = typeof conversation?.title === 'string' && conversation.title.trim()
      ? conversation.title.trim()
      : `对话 #${conversation?.id}`
    try {
      const notification = new notificationApi('Assistant 对话已完成', {
        body: title,
        icon: NOTIFICATION_ICON,
        tag: `assistant-conversation-${conversation?.id}`,
      })
      notification.onclick = () => {
        globalThis.window?.focus()
        notification.close()
        onOpenConversation?.(conversation?.id)
      }
      return notification
    } catch (err) {
      console.error('Failed to show conversation notification', err)
      return null
    }
  }

  return { requestPermission, notifyConversationCompleted }
}
