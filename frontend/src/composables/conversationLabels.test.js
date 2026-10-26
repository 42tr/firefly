import assert from 'node:assert/strict'
import test from 'node:test'
import { conversationCreationDirectoryLabel, conversationCreationDirectoryPath, hasCreationDirectory } from '../utils/conversationLabels.js'

test('conversation directory prefixes retain the original creation directory of parallel copies', () => {
  const conversation = { cwd: '.assistant-parallel/copy/work', source_cwd: 'projects/firefly', workspace_mode: 'parallel' }
  assert.equal(conversationCreationDirectoryLabel(conversation), 'firefly')
  assert.equal(conversationCreationDirectoryPath(conversation), 'workspace/projects/firefly')
  assert.equal(conversation.cwd, '.assistant-parallel/copy/work')
})

test('conversation directory prefixes cover ordinary paths, empty sources and default roots', () => {
  for (const [conversation, label, path] of [
    [{ cwd: '/workspace/笔记' }, '笔记', '/workspace/笔记'],
    [{ cwd: 'projects/firefly', source_cwd: '  ' }, 'firefly', 'workspace/projects/firefly'],
    [{ cwd: '.' }, 'workspace', 'workspace'],
    [{ agent: 'firefly' }, 'workspace', 'workspace'],
  ]) {
    assert.equal(conversationCreationDirectoryLabel(conversation), label)
    assert.equal(conversationCreationDirectoryPath(conversation), path)
  }
})

test('sidebar hides the directory label when the conversation has no real creation directory', () => {
  for (const conversation of [{}, { agent: 'firefly' }, { cwd: '' }, { cwd: '.' }, { source_cwd: '   ' }]) {
    assert.equal(hasCreationDirectory(conversation), false)
  }
  for (const conversation of [{ cwd: '/workspace/笔记' }, { cwd: 'tools/codex-tools' }, { source_cwd: 'projects/firefly' }]) {
    assert.equal(hasCreationDirectory(conversation), true)
  }
})
