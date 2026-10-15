import test from 'node:test'
import assert from 'node:assert/strict'
import { taskTree, activeTask, activeBranch, taskStatus } from './taskTree.js'

test('workflow retains actual multi-level parents and independent branches', () => {
  const tree = taskTree([{ id: 'B', parent_task_id: 'A' }, { id: 'C' }, { id: 'A' }, { id: 'D', parent_task_id: 'B' }])
  assert.deepEqual(tree.map(n => n.id), ['C', 'A'])
  assert.equal(tree[1].children[0].children[0].id, 'D')
})
test('missing parents and cycles remain visible without recursive loops', () => {
  const tree = taskTree([{ id: 'A', parent_task_id: 'B' }, { id: 'B', parent_task_id: 'A' }, { id: 'C', parent_task_id: 'gone' }])
  assert.equal(tree.length, 3)
  assert.ok(tree.every(node => node.children.length === 0))
})
test('detached execution remains active after a reply and cancellation is not completion', () => {
  assert.equal(taskStatus({ status: 'running', detached: true }), '后台执行中')
  assert.equal(activeTask({ status: 'cancelling' }), true)
  assert.equal(activeTask({ status: 'unknown' }), false)
  assert.equal(taskStatus({ status: 'unknown' }), '结果未知')
  assert.equal(activeBranch({ status: 'succeeded', children: [{ status: 'running' }] }), true)
})
