// Queue submissions and streamed replies can be interleaved in storage. Keep
// each turn together for display without changing the stream's message indices.
export function orderMessagesByTurn(messages, { includeQueued = false } = {}) {
  const userJobs = new Set(messages.filter((message) => message.role === 'user')
    .map((message) => message.generationJobId || message.jobId))
  const turns = new Map()
  const earlierTurns = []
  const ordered = []
  for (const message of messages) {
    // Whitespace-only stream fragments have no visible content. Keeping one
    // between tool calls would split a single process into separate cards.
    // Filter only the display list; stream handlers still need the source indices.
    if (message.role === 'assistant' && !message.kind && !message.chartOption
      && !message.title && !message.attachments?.length && !message.content?.trim()) continue
    if (!includeQueued && message.role === 'user' && ['submitting', 'queued'].includes(message.processingStatus)) continue
    const jobId = message.generationJobId || message.jobId
    if (!jobId) {
      ordered.push([message])
      continue
    }
    let turn = turns.get(jobId)
    if (!turn) {
      turn = []
      turns.set(jobId, turn)
      // With ID-based history pagination, a turn's question may be on an
      // older page while its final reply follows a newer queued question.
      if (userJobs.has(jobId)) ordered.push(turn)
      else earlierTurns.push(turn)
    }
    turn.push(message)
  }
  return [...earlierTurns, ...ordered].flat()
}
