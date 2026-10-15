import assert from 'node:assert/strict'
import test from 'node:test'
import { getProcessDurationMs } from './processDuration.js'

test('process duration spans the earliest start and latest completion', () => {
  const steps = [
    { startedAt: '2026-09-09T00:00:01.000Z', completedAt: '2026-09-09T00:00:03.000Z' },
    { startedAt: '2026-09-09T00:00:04.000Z', completedAt: '2026-09-09T00:01:06.000Z' },
  ]
  assert.equal(getProcessDurationMs(steps, false), 65_000)
})

test('running process duration uses the current time and falls back to message time', () => {
  const steps = [{ time: '2026-09-09T00:00:01.000Z', status: 'running' }]
  const now = new Date('2026-09-09T00:00:12.000Z').getTime()
  assert.equal(getProcessDurationMs(steps, true, now), 11_000)
})

test('duration is unavailable without a valid timestamp', () => {
  assert.equal(getProcessDurationMs([{ status: 'completed' }], false), null)
})

test('persisted job boundaries override batched message save times and exclude queue waiting', () => {
  const steps = [
    { time: '2026-10-12T01:07:00Z', startedAt: '2026-10-12T01:07:00Z', completedAt: '2026-10-12T01:07:00Z' },
    { kind: 'job-timing', startedAt: '2026-10-12T01:00:00Z', completedAt: '2026-10-12T01:07:00Z' },
  ]
  assert.equal(getProcessDurationMs(steps, false), 420000)
  assert.equal(getProcessDurationMs(steps, true, Date.parse('2026-10-12T01:08:00Z')), 480000)
})

test('legacy durations include the final reply and invalid explicit times fall back to recorded times', () => {
  assert.equal(getProcessDurationMs([
    { startedAt: 'invalid', time: '2026-10-12T01:00:00Z', completedAt: '2026-10-12T01:01:00Z' },
    { time: '2026-10-12T01:03:00Z' },
  ], false), 180000)
  assert.equal(getProcessDurationMs([{ startedAt: '2026-10-12T01:00:00Z' }], false), null)
})
