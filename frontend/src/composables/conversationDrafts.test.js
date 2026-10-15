import assert from 'node:assert/strict'
import test from 'node:test'
import { createConversationDrafts } from './conversationDrafts.js'

test('drafts isolate attachments by conversation and restore only text after reload', () => {
  const values = new Map()
  const storage = { getItem: key => values.get(key), setItem: (key, value) => values.set(key, value) }
  const drafts = createConversationDrafts(storage)
  const file = new File(['content'], 'a.txt')
  drafts.save(1, 'draft A', [{ file, name: 'a.txt' }])
  drafts.save(2, 'draft B', [])
  assert.equal(drafts.has(1), true)
  assert.equal(drafts.has(2), true)
  assert.equal(drafts.has(3), false)
  drafts.save(3, '', [{ file, name: 'a.txt' }])
  assert.equal(drafts.has(3), true)
  assert.equal(createConversationDrafts(storage).has(3), false)
  assert.deepEqual(drafts.restore(2), { text: 'draft B', attachments: [] })
  assert.equal(drafts.restore(1).attachments[0].file, file)
  assert.deepEqual(createConversationDrafts(storage).restore(1), { text: 'draft A', attachments: [] })
  drafts.save(1, '', [])
  assert.equal(drafts.has(1), false)
  assert.equal(createConversationDrafts(storage).restore(1).text, '')
  drafts.delete(2)
  assert.equal(drafts.has(2), false)
  assert.equal(createConversationDrafts(storage).restore(2).text, '')
})

test('drafts remain usable when browser storage is invalid or unavailable', () => {
  for (const saved of ['broken', 'null', '42', '[]']) {
    const drafts = createConversationDrafts({ getItem: () => saved, setItem: () => { throw new Error('storage blocked') } })
    drafts.save(1, 'retained', [])
    assert.equal(drafts.restore(1).text, 'retained')
  }
})
