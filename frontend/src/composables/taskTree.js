export const activeTask = task => ['queued', 'running', 'cancelling'].includes(task.status)
export const activeBranch = node => activeTask(node) || (node.children || []).some(activeBranch)
export const taskStatus = task => ({
  queued: '排队中', running: task.detached ? '后台执行中' : '执行中', cancelling: '取消中',
  succeeded: '已完成', failed: '失败', cancelled: '已取消', timed_out: '执行超时', unknown: '结果未知',
}[task.status] || task.status)

export function taskTree(tasks) {
  const nodes = new Map(tasks.map(task => [task.id, { ...task, children: [] }]))
  const roots = []
  for (const node of nodes.values()) {
    const parent = nodes.get(node.parent_task_id)
    const seen = new Set([node.id])
    let ancestor = parent
    while (ancestor && !seen.has(ancestor.id)) { seen.add(ancestor.id); ancestor = nodes.get(ancestor.parent_task_id) }
    if (parent && !ancestor) parent.children.push(node)
    else roots.push(node)
  }
  return roots
}
