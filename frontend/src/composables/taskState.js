export const activeTask = task => ['queued', 'running', 'cancelling'].includes(task.status)
