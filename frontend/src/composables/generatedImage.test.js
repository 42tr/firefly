import assert from 'node:assert/strict'
import test from 'node:test'
import { generatedImageOutput } from './generatedImage.js'

test('only successful native image results with attachment URLs are image cards', () => {
  const result = { type: 'image', image_url: '/api/assistant/attachments/by-file/file', failure: null }
  assert.equal(generatedImageOutput({ name: 'image_generation', result }), result)
  for (const payload of [
    { name: 'command', result }, { name: 'image_generation', result, is_error: true },
    { name: 'image_generation', result: { ...result, failure: { code: 'failed' } } },
    { name: 'image_generation', result: { ...result, error: 'missing file' } },
    { name: 'image_generation', result: { type: 'image', saved_path: '/root/.codex/generated_images/file.png' } },
    { name: 'image_generation', result: { ...result, image_url: 'file:///etc/passwd' } },
  ]) assert.equal(generatedImageOutput(payload), null)
})
