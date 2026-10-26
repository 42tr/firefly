import assert from 'node:assert/strict'
import test from 'node:test'
import { agentLabel, conversationAgentLabel, conversationDirectoryLabel } from '../utils/conversationLabels.js'

test('agent labels cover known agents and fallback values', () => {
  assert.equal(agentLabel('firefly'), 'Firefly')
  assert.equal(agentLabel('codex'), 'Codex')
  assert.equal(agentLabel('claude'), 'Claude')
  assert.equal(agentLabel('other'), 'other')
  assert.equal(agentLabel(undefined), 'Agent')
})

test('conversation agent labels use the conversation agent', () => {
  assert.equal(conversationAgentLabel({ agent: 'codex' }), 'Codex')
  assert.equal(conversationAgentLabel({ agent: 'unknown' }), 'unknown')
  assert.equal(conversationAgentLabel({}), 'Agent')
})

test('conversation directory labels cover ordinary paths and default roots', () => {
  for (const [conversation, label] of [
    [{ cwd: '/workspace/笔记' }, '笔记'],
    [{ cwd: 'projects/firefly' }, 'firefly'],
    [{ cwd: '.' }, 'workspace'],
    [{ agent: 'firefly' }, ''],
    [{}, ''],
  ]) {
    assert.equal(conversationDirectoryLabel(conversation), label)
  }
})
