import test from 'node:test'
import assert from 'node:assert/strict'
import { activeTask } from './taskState.js'

test('queued, running and cancelling tasks are active', () => {
  for (const status of ['queued', 'running', 'cancelling']) assert.equal(activeTask({ status }), true, status)
})
test('finished and unknown tasks are not active', () => {
  for (const status of ['succeeded', 'failed', 'cancelled', 'timed_out', 'unknown']) assert.equal(activeTask({ status }), false, status)
})
