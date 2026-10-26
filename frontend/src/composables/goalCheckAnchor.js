// Keep the latest check beside its Goal reply, even when ordinary chat follows.
// If that Goal is outside the loaded history, do not attach it to another turn.
export function goalCheckAnchor(items, goal, conversationId) {
  if (!goal?.last_check_result || goal.id == null) return null
  if (goal.conversation_id != null && String(goal.conversation_id) !== String(conversationId)) return null
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index]
    if (item.type === 'msg' && item.msg.role === 'assistant'
      && item.msg.goal?.goal_id != null
      && String(item.msg.goal.goal_id) === String(goal.id)) return item.key
  }
  return null
}
