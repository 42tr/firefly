"""Workspace HTML preview regression: sandbox attributes, inline rendering and size guards.

Build: npm --prefix frontend run build && cargo build --locked
Run: python3 frontend/tests/e2e/test_html_preview_ui.py
Uses an isolated host; the sandboxed iframe is loaded from the real preview endpoint.
"""
import os
import unittest
from pathlib import Path

from playwright.sync_api import expect
import test_modules_ui as harness

CASE = """<!doctype html><html lang="zh"><head><meta charset="utf-8"><title>预览用例</title>
<style>body{font:16px/1.5 system-ui;margin:0;padding:24px}</style></head>
<body><h1 id="title">初始</h1><p id="count"></p>
<script>
document.getElementById('title').textContent='沙箱内脚本已执行';
document.getElementById('count').textContent='条目 '+document.title;
</script></body></html>
"""
SANDBOX = 'allow-scripts allow-forms allow-downloads'


class HtmlPreviewUI(harness.ModuleHost):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        directory = cls.directory / 'workspace' / 'preview-case'
        directory.mkdir(parents=True, exist_ok=True)
        (directory / 'case.html').write_text(CASE, encoding='utf-8')
        (directory / 'notes.txt').write_text('纯文本', encoding='utf-8')
        # 8 MiB + 1 byte 用来验证前端体积预检。
        with open(directory / 'big.html', 'wb') as handle:
            handle.write(b'<!doctype html><p>x</p>')
            handle.truncate(8 * 1024 * 1024 + 1)

    def setUp(self):
        self.context = self.browser.new_context(viewport={'width': 1280, 'height': 900})
        self.addCleanup(self.context.close)
        headers = {'Origin': self.base}
        response = self.context.request.post(self.base + '/auth', headers=headers, data={'password': 'module-test-password'})
        self.assertTrue(response.ok, response.text())
        response = self.context.request.post(self.base + '/api/assistant/conversations', headers=headers,
                                             data={'agent': 'codex', 'title': 'HTML 预览', 'cwd': 'preview-case'})
        self.assertTrue(response.ok, response.text())
        self.conversation = response.json()['id']
        self.page = self.context.new_page()
        self.errors = []
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.page.goto(f'{self.base}/?conversation={self.conversation}')
        expect(self.page.get_by_role('button', name='更多功能', exact=True)).to_be_visible(timeout=15000)
        self.open_files()

    def tearDown(self):
        self.assertEqual(self.errors, [])

    def open_files(self):
        self.page.get_by_role('button', name='更多功能', exact=True).click()
        self.page.get_by_role('menuitem', name='文件', exact=True).click()
        self.drawer = self.page.locator('dialog.files-drawer')
        expect(self.drawer).to_be_visible(timeout=15000)
        expect(self.drawer.locator('.file-name', has_text='case.html')).to_be_visible(timeout=15000)
        return self.drawer

    def row(self, name):
        return self.drawer.locator('.file-row').filter(has_text=name)

    def test_inline_html_renders_in_sandbox_and_blocks_parent_access(self):
        action = self.row('case.html').get_by_role('button', name='在应用内预览 case.html')
        expect(action).to_have_count(1)
        expect(action).to_be_visible()
        action.click()
        dialog = self.page.locator('dialog.html-preview')
        expect(dialog).to_be_visible(timeout=15000)
        frame_element = dialog.locator('iframe')
        expect(frame_element).to_have_count(1)
        self.assertEqual(frame_element.get_attribute('sandbox'), SANDBOX)
        self.assertNotIn('allow-same-origin', frame_element.get_attribute('sandbox'))
        self.assertEqual(frame_element.get_attribute('referrerpolicy'), 'no-referrer')
        expect(dialog.get_by_role('heading', name='case.html')).to_be_visible()

        frame = [item for item in self.page.frames if '/files/preview' in (item.url or '')][-1]
        expect(frame.locator('#title')).to_have_text('沙箱内脚本已执行')
        self.assertEqual(frame.locator('#count').inner_text(), '条目 预览用例')
        self.assertEqual(frame.evaluate("() => { try { return String(window.origin) } catch (error) { return 'THROW:' + error.name } }"), 'null')
        self.assertIn('SecurityError', frame.evaluate("() => { try { return 'COOKIE:' + document.cookie } catch (error) { return 'THROW:' + error.name } }"))
        self.assertIn('SecurityError', frame.evaluate("() => { try { return window.parent.document ? 'ACCESSIBLE' : 'NULL' } catch (error) { return 'BLOCKED:' + error.name } }"))
        self.assertIn('BLOCKED', frame.evaluate("async () => { try { await fetch('/api/assistant/conversations'); return 'ALLOWED' } catch (error) { return 'BLOCKED:' + error.name } }"))

        dialog.get_by_role('button', name='关闭 HTML 预览').click()
        expect(dialog).to_be_hidden()
        expect(dialog.locator('iframe')).to_have_count(0)
        expect(self.drawer).to_be_visible()

    def test_non_html_has_no_preview_and_oversize_is_disabled(self):
        expect(self.row('notes.txt').get_by_role('button', name='在应用内预览 notes.txt')).to_have_count(0)
        # 文本文件继续走原有内联文本预览，不受 HTML 预览影响。
        expect(self.row('notes.txt').get_by_role('button', name='预览 notes.txt')).to_have_count(1)
        big = self.row('big.html').get_by_role('button', name='超过 8MB，请下载查看')
        expect(big).to_be_disabled()
        expect(big).to_have_text('预览')
        expect(big).to_have_attribute('title', '超过 8MB，请下载查看')

    def test_filename_opens_preview_and_escape_layers(self):
        self.row('case.html').locator('button.file-name').click()
        dialog = self.page.locator('dialog.html-preview')
        expect(dialog).to_be_visible(timeout=15000)
        expect(dialog.locator('iframe')).to_have_count(1)
        self.page.keyboard.press('Escape')
        expect(dialog).to_be_hidden()
        expect(self.drawer).to_be_visible()


if __name__ == '__main__':
    unittest.main(verbosity=2)
