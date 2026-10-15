export function getAppTimezone() {
  const configured = typeof document === 'undefined' ? null : document.querySelector?.('meta[name="firefly-timezone"]')?.content
  return configured || Intl.DateTimeFormat().resolvedOptions().timeZone
}

export function formatAppDateTime(value, options = {}) {
  return new Date(value).toLocaleString('zh-CN', { hourCycle: 'h23', ...options, timeZone: getAppTimezone() })
}

export function appDateParts(value, timeZone = getAppTimezone()) {
  const parts = new Intl.DateTimeFormat('en-CA', {
    timeZone, calendar: 'gregory', numberingSystem: 'latn', hourCycle: 'h23',
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit',
  }).formatToParts(value)
  return Object.fromEntries(parts.map(({ type, value }) => [type, value]))
}
