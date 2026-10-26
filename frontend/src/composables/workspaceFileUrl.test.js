import assert from 'node:assert/strict'
import test from 'node:test'
import { workspaceFileUrl } from './workspaceFileUrl.js'

const params = url => new URL(url, 'https://assistant.example').searchParams

test('extensionless uploads use the local attachment endpoint', () => {
  const file = '1fb5ecfa-6bf0-475e-a524-ee190cf3e9b0'
  for (const cwd of ['', 'firefly']) {
    for (const prefix of ['/srv/firefly/storage/', 'storage/']) {
      const path = `${prefix}assistant_uploads/${file}`
      for (const input of [path, `sandbox:${path}`, ...(path.startsWith('/') ? [`file://${path}`] : [])]) {
        assert.equal(workspaceFileUrl(input, { cwd }), `/api/assistant/attachments/by-file/${file}`)
      }
    }
  }
  assert.notEqual(workspaceFileUrl(`/other/${file}`), `/api/assistant/attachments/by-file/${file}`)
  assert.notEqual(workspaceFileUrl(`storage/assistant_uploads/../${file}`), `/api/assistant/attachments/by-file/${file}`)
})

test('temporary images keep their absolute path, including nested workspace directories', () => {
  for (const path of ['/tmp/image.png', '/tmp/workspace/image.png', '/tmp/图片 1.png']) {
    for (const input of [path, encodeURI(path), `sandbox:${path}`, `file://${encodeURI(path)}`]) {
      const query = params(workspaceFileUrl(input, { cwd: 'project' }))
      assert.equal(query.get('path'), path)
    }
  }
})

test('workspace images preserve local relative paths', () => {
  assert.equal(params(workspaceFileUrl('/disk/workspace/project/a.png')).get('path'), 'project/a.png')
  assert.equal(params(workspaceFileUrl('firefly/workspace/project/a.png')).get('path'), 'project/a.png')
  assert.equal(params(workspaceFileUrl('workspace/project/a.png')).get('path'), 'project/a.png')
  assert.equal(params(workspaceFileUrl('./output/a.png', { cwd: 'project' })).get('path'), 'project/output/a.png')
  assert.equal(params(workspaceFileUrl('./output/a.png', { cwd: 'firefly/workspace' })).get('path'), 'output/a.png')
})

test('browser URLs are preserved and malformed or unsupported protocols are rejected', () => {
  for (const path of ['https://example.com/a%20b.png', 'data:image/png;base64,abcd', 'blob:https://example.com/id', '/api/assistant/workspace/file?path=%2Ftmp%2Fa.png']) {
    assert.equal(workspaceFileUrl(path), path)
  }
  for (const path of ['', '%broken', 'javascript:alert(1)', 'file://elsewhere/tmp/a.png', '//elsewhere/tmp/a.png']) {
    assert.equal(workspaceFileUrl(path), '')
  }
})
