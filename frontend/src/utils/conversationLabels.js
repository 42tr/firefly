const agentNames = { firefly: 'Firefly', codex: 'Codex', claude: 'Claude' }

export const agentLabel = agent => agentNames[agent] || agent || 'Agent'

export const conversationAgentLabel = conversation => agentLabel(conversation?.agent)

export const conversationDirectoryLabel = conversation => {
  if (!conversation?.cwd && !['codex', 'claude'].includes(conversation?.agent)) return ''
  const cwd = String(conversation.cwd || '')
  return cwd.split('/').filter(part => part && part !== '.').at(-1) || 'workspace'
}

const creationDirectory = conversation => String(conversation?.source_cwd || '').trim()
  || String(conversation?.cwd || '').trim()

export const conversationCreationDirectoryLabel = conversation => {
  const directory = creationDirectory(conversation)
  return directory.split('/').filter(part => part && part !== '.').at(-1) || 'workspace'
}

export const conversationCreationDirectoryPath = conversation => {
  const directory = creationDirectory(conversation)
  if (!directory || directory === '.') return 'workspace'
  return directory.startsWith('/') ? directory : `workspace/${directory}`
}
