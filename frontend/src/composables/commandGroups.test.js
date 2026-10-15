import assert from 'node:assert/strict'
import test from 'node:test'
import { groupConsecutiveCommands, isCommandRun } from './commandGroups.js'

const command = (id, extra = {}) => ({ id, kind: 'tool-run', name: 'command', status: 'completed', ...extra })

test('groups adjacent tool calls without crossing commentary, handoffs, or jobs', () => {
  const steps = [
    command('a'), command('b'),
    { role: 'assistant', content: '检查结果' },
    command('c'),
    { kind: 'handoff', agent: 'research' },
    command('d', { jobId: 'one' }), command('e', { jobId: 'two' }),
  ]
  const groups = groupConsecutiveCommands(steps)
  assert.deepEqual(groups.map(group => group.commands?.map(item => item.id) ?? group), [
    ['a', 'b'], steps[2], ['c'], steps[4], ['d'], ['e'],
  ])
  assert.equal(groups[0].commands[0], steps[0])
  assert.equal(steps[0].kind, 'tool-run')
  assert.deepEqual(groupConsecutiveCommands([]), [])
})

test('read, bash and other tools join commands and file changes in order', () => {
  const steps = ['Read', 'Bash', 'command', 'file_change', 'read', 'bash', 'Grep', 'mcp_tool']
    .map((name, index) => command(String(index), { name }))
  const groups = groupConsecutiveCommands(steps)
  assert.equal(groups.length, 1)
  assert.deepEqual(groups[0].commands, steps)
  assert.deepEqual(steps.filter(isCommandRun).map(step => step.name), ['Bash', 'command', 'bash'])
})

test('commands and file changes share a group in their original order', () => {
  const steps = [command('a'), command('b', { name: 'file_change' }), command('c'), command('d', { name: 'file_change' })]
  const groups = groupConsecutiveCommands(steps)
  assert.equal(groups.length, 1)
  assert.deepEqual(groups[0].commands, steps)
  const filesOnly = groupConsecutiveCommands(steps.filter(step => step.name === 'file_change'))
  assert.equal(filesOnly.length, 1)
  assert.equal(filesOnly[0].commands.length, 2)
})

test('appending a file change preserves an expanded command group identity', () => {
  const steps = [command('a')]
  const first = groupConsecutiveCommands(steps)[0]
  steps.push(command('b', { name: 'file_change', status: 'running' }))
  assert.equal(groupConsecutiveCommands(steps)[0].id, first.id)
})

test('streaming append preserves group identity and original command updates', () => {
  const steps = [command('a', { status: 'running' })]
  const first = groupConsecutiveCommands(steps)[0]
  steps[0].status = 'completed'
  steps.push(command('b', { status: 'running' }))
  const next = groupConsecutiveCommands(steps)[0]
  assert.equal(first.id, next.id)
  assert.equal(next.commands.length, 2)
  assert.equal(first.commands[0].status, 'completed')
})
