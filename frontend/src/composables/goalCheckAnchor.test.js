import assert from 'node:assert/strict'
import test from 'node:test'
import { goalCheckAnchor } from './goalCheckAnchor.js'

const goal = { id: 7, conversation_id: 1, last_check_result: { status: 'completed' } }
const message = (key, role, goalId) => ({ type: 'msg', key, msg: { role, goal: goalId == null ? null : { goal_id: goalId } } })

test('check stays with the Goal result when ordinary messages follow', () => {
  const items = [message('start', 'user', 7), message('reply', 'assistant', 7), message('summary', 'assistant', 7)]
  assert.equal(goalCheckAnchor(items, goal, 1), 'summary')
  items.push(message('question', 'user'), { type: 'process', key: 'process' }, message('answer', 'assistant'))
  assert.equal(goalCheckAnchor(items, goal, 1), 'summary')
})

test('matches only the current Goal and supports serialized IDs', () => {
  const items = [message('reply', 'assistant', '7'), message('other', 'assistant', 8)]
  assert.equal(goalCheckAnchor(items, goal, 1), 'reply')
  assert.equal(goalCheckAnchor(items, { ...goal, id: 8 }, 1), 'other')
})

test('does not show orphaned or stale checks', () => {
  assert.equal(goalCheckAnchor([message('start', 'user', 7)], goal, 1), null)
  assert.equal(goalCheckAnchor([], goal, 1), null)
  assert.equal(goalCheckAnchor([message('reply', 'assistant', 7)], goal, 2), null)
  assert.equal(goalCheckAnchor([], null, 1), null)
  assert.equal(goalCheckAnchor([message('reply', 'assistant', 7)], { id: 7 }, 1), null)
})
