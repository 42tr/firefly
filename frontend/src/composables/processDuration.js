const timestamp = (value) => {
  if (value == null || value === '') return null
  const millis = new Date(value).getTime()
  return Number.isFinite(millis) ? millis : null
}

export function getProcessDurationMs(steps, running, now = Date.now()) {
  const starts = []
  const ends = []
  for (const step of steps || []) {
    const start = timestamp(step.startedAt) ?? timestamp(step.time)
    const end = timestamp(step.completedAt) ?? timestamp(step.time)
    if (start !== null) starts.push(start)
    if (end !== null) ends.push(end)
  }
  const timing = steps?.find(step => step.kind === 'job-timing')
  const start = timestamp(timing?.startedAt) ?? (starts.length ? Math.min(...starts) : null)
  const end = running ? now : timestamp(timing?.completedAt) ?? (ends.length ? Math.max(...ends) : null)
  if (start === null || end === null) return null
  return Math.max(0, end - start)
}
