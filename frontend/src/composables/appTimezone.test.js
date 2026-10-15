import assert from 'node:assert/strict'
import test from 'node:test'
import { appDateParts, formatAppDateTime, getAppTimezone } from './appTimezone.js'

test('server configuration controls display even when the browser has another timezone', t => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'document')
  Object.defineProperty(globalThis, 'document', { configurable: true, value: { querySelector: () => ({ content: 'America/New_York' }) } })
  t.after(() => { if (previous) Object.defineProperty(globalThis, 'document', previous); else delete globalThis.document })
  assert.equal(getAppTimezone(), 'America/New_York')
  assert.equal(formatAppDateTime('2026-03-08T13:00:00Z'), new Date('2026-03-08T13:00:00Z').toLocaleString('zh-CN', { hourCycle: 'h23', timeZone: 'America/New_York' }))
})

test('calendar dates and DST offsets follow the selected zone', () => {
  const midnight = appDateParts(new Date('2026-03-08T01:00:00Z'), 'America/New_York')
  assert.equal(midnight.day, '07')
  assert.equal(midnight.hour, '20')
  assert.equal(appDateParts(new Date('2026-03-08T13:00:00Z'), 'America/New_York').hour, '09')
  assert.equal(appDateParts(new Date('2026-11-01T13:00:00Z'), 'America/New_York').hour, '08')
})
