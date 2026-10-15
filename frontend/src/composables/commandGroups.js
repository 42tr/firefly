// Group adjacent tool calls; retain the original objects for streaming updates.
export function groupConsecutiveCommands(steps = []) {
  const groups = []
  steps.forEach((step, index) => {
    if (step.kind !== 'tool-run') {
      groups.push(step)
      return
    }
    const previous = groups.at(-1)
    if (previous?.kind === 'command-group' && previous.jobId === step.jobId) {
      previous.commands.push(step)
    } else {
      groups.push({
        kind: 'command-group',
        id: `commands-${step.id ?? index}`,
        jobId: step.jobId,
        commands: [step],
      })
    }
  })
  return groups
}

export const isCommandRun = (step) => ['command', 'bash'].includes(step.name?.toLowerCase())
