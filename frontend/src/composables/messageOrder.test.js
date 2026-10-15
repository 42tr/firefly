import assert from 'node:assert/strict'
import test from 'node:test'
import { orderMessagesByTurn } from './messageOrder.js'

const user = (id, processingStatus = 'completed') => ({
  role: 'user', content: `question ${id}`, generationJobId: id, processingStatus,
})
const reply = (id, content) => ({ role: 'assistant', jobId: id, content })

test('blank stream fragments do not separate tools into multiple process cards', () => {
  const question = user('a')
  const firstTools = Array.from({ length: 5 }, () => ({ role: 'tool', jobId: 'a' }))
  const blank = reply('a', '\n  \t')
  const lastTools = Array.from({ length: 4 }, () => ({ role: 'tool', jobId: 'a' }))
  const final = reply('a', '完成')
  const messages = [question, ...firstTools, blank, ...lastTools, final]
  assert.deepEqual(orderMessagesByTurn(messages), [question, ...firstTools, ...lastTools, final])
  assert.equal(messages[6], blank, 'stream indices must stay unchanged')
  blank.content = '继续检查'
  assert.ok(orderMessagesByTurn(messages).includes(blank), 'real commentary must remain visible')
})

test('messages with images, charts, bills, or titles survive empty-text filtering', () => {
  const visible = [
    { ...reply('a', ''), attachments: [{ name: 'image.png' }] },
    { ...reply('a', ''), chartOption: { series: [] } },
    { ...reply('a', ''), kind: 'bill' },
    { ...reply('a', ''), title: '结果' },
  ]
  assert.deepEqual(orderMessagesByTurn(visible), visible)
})

test('the next queued turn appears after the previous final reply when it starts', () => {
  const question = user('a')
  const tool = { role: 'tool', jobId: 'a' }
  const next = user('b', 'queued')
  const final = reply('a', '已提交并推送')
  const messages = [question, tool, next, final]
  assert.deepEqual(orderMessagesByTurn(messages), [question, tool, final])

  next.processingStatus = 'running'
  const nextReply = reply('b', '检查 Actions 耗时')
  messages.push(nextReply)
  assert.deepEqual(orderMessagesByTurn(messages), [question, tool, final, next, nextReply])
  // The stream still uses indices into the original array.
  assert.equal(messages[3], final)
  assert.equal(orderMessagesByTurn(messages)[2], final)
})

test('reloaded history keeps multiple queued turns and their process steps together', () => {
  const a = user('a')
  const b = user('b')
  const c = user('c', 'failed')
  const commentary = reply('a', 'working')
  const tool = { role: 'tool', jobId: 'a' }
  const finalA = reply('a', 'done a')
  const finalB = reply('b', 'done b')
  const errorC = reply('c', 'error c')
  const history = [a, commentary, b, tool, c, finalA, finalB, errorC]
  assert.deepEqual(orderMessagesByTurn(history), [a, commentary, tool, finalA, b, finalB, c, errorC])
})

test('pending inputs do not split a reply; legacy messages retain their order', () => {
  const legacyUser = { role: 'user', content: 'old question' }
  const legacyReply = { role: 'assistant', content: 'old answer' }
  const a = user('a', 'running')
  const first = reply('a', 'first')
  const last = reply('a', 'last')
  const pending = user(null, 'submitting')
  assert.deepEqual(
    orderMessagesByTurn([legacyUser, legacyReply, a, first, pending, user('b', 'queued'), last]),
    [legacyUser, legacyReply, a, first, last],
  )
})

test('a page starting with a queued question still shows the preceding turn first', () => {
  const a = user('a')
  const b = user('b')
  const finalA = reply('a', 'done a')
  const finalB = reply('b', 'done b')
  const page = [b, finalA, finalB]
  assert.deepEqual(orderMessagesByTurn(page), [finalA, b, finalB])
  assert.deepEqual(orderMessagesByTurn([a, ...page]), [a, finalA, b, finalB])
})
