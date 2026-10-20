"""Workspace completion regression; start Vite on 5187 before running this file."""
import json
import unittest
from urllib.parse import urlparse, parse_qs
from playwright.sync_api import expect
import test_parallel_workspace_ui as fixtures


class WorkspaceCompletionUI(unittest.TestCase):
    setUp = fixtures.ParallelWorkspaceUI.setUp
    tearDown = fixtures.ParallelWorkspaceUI.tearDown
    conversation = fixtures.ParallelWorkspaceUI.conversation
    open = fixtures.ParallelWorkspaceUI.open

    def api(self, route):
        url = urlparse(route.request.url)
        if url.path.endswith('/workspace'):
            query = parse_qs(url.query)
            path = query.get('path', [''])[0]
            names = ['app', '项目 资料', 'zebra'] if not path else ['src'] if path == 'app' else []
            result = dict(root='/srv/workspace', path=path, entries=[
                dict(name=name, path=f'{path}/{name}' if path else name) for name in names])
            route.fulfill(content_type='application/json', body=json.dumps(result))
        else:
            fixtures.ParallelWorkspaceUI.api(self, route)

    def composer(self):
        self.conversations = [self.conversation()]
        self.open(conversation=True)
        return self.page.locator('.conversation-column textarea.chat-input')

    def test_keyboard_completion_preserves_surrounding_text_and_does_not_send(self):
        composer = self.composer()
        composer.fill('请检查 ///，保留后文')
        composer.evaluate('(el) => el.setSelectionRange(7, 7)')
        composer.press('ArrowLeft')
        composer.press('ArrowRight')
        expect(self.page.get_by_role('option')).to_have_count(3)
        composer.press('ArrowDown')
        composer.press('Enter')
        expect(composer).to_have_value('请检查 /srv/workspace/项目 资料，保留后文')
        expect(self.page.get_by_role('listbox')).to_have_count(0)
        self.assertEqual(composer.evaluate('(el) => el.selectionStart'), len('请检查 /srv/workspace/项目 资料'))

    def test_filter_nested_directory_and_tab(self):
        composer = self.composer()
        composer.fill('检查 ///ap')
        expect(self.page.get_by_role('option')).to_have_count(1)
        composer.press('Tab')
        expect(composer).to_have_value('检查 ///app/')
        expect(composer).to_be_focused()
        expect(self.page.get_by_role('option', name='src')).to_be_visible()
        composer.press('Enter')
        expect(composer).to_have_value('检查 /srv/workspace/app/src')
        expect(self.page.get_by_role('listbox')).to_have_count(0)

    def test_repeated_tab_enters_leaf_and_enter_confirms_it(self):
        composer = self.composer()
        composer.fill('///ap')
        expect(self.page.get_by_role('option', name='app')).to_be_visible()
        composer.press('Tab')
        expect(self.page.get_by_role('option', name='src')).to_be_visible()
        composer.press('Tab')
        expect(composer).to_have_value('///app/src/')
        expect(self.page.locator('.directory-completion').get_by_role('status')).to_have_text('没有匹配的目录')
        composer.press('Tab')
        expect(composer).to_be_focused()
        expect(composer).to_have_value('///app/src/')
        composer.press('Enter')
        expect(composer).to_have_value('/srv/workspace/app/src')
        expect(self.page.get_by_role('listbox')).to_have_count(0)

    def test_tab_preserves_surrounding_text(self):
        composer = self.composer()
        composer.fill('请检查 ///，保留后文')
        composer.evaluate('(el) => el.setSelectionRange(7, 7)')
        composer.press('ArrowLeft')
        composer.press('ArrowRight')
        expect(self.page.get_by_role('option')).to_have_count(3)
        composer.press('Tab')
        expect(composer).to_have_value('请检查 ///app/，保留后文')
        expect(self.page.get_by_role('option', name='src')).to_be_visible()
        composer.press('Enter')
        expect(composer).to_have_value('请检查 /srv/workspace/app/src，保留后文')

    def test_mouse_chinese_directory_and_escape(self):
        composer = self.composer()
        composer.fill('///项目')
        self.page.get_by_role('option', name='项目 资料').click()
        expect(composer).to_have_value('/srv/workspace/项目 资料')
        composer.fill('///')
        expect(self.page.get_by_role('option')).to_have_count(3)
        composer.press('Escape')
        expect(self.page.get_by_role('listbox')).to_have_count(0)
        expect(composer).to_have_value('///')

    def test_empty_results_do_not_send_and_url_does_not_trigger(self):
        composer = self.composer()
        composer.fill('///missing')
        expect(self.page.locator('.directory-completion').get_by_role('status')).to_have_text('没有匹配的目录')
        composer.press('Enter')
        expect(composer).to_have_value('///missing')
        composer.fill('https:///example.com')
        expect(self.page.get_by_role('listbox')).to_have_count(0)

    def test_ime_enter_does_not_select_or_send(self):
        composer = self.composer()
        composer.fill('///')
        expect(self.page.get_by_role('option')).to_have_count(3)
        composer.dispatch_event('keydown', {'key': 'Enter', 'isComposing': True})
        expect(composer).to_have_value('///')
        expect(self.page.get_by_role('option')).to_have_count(3)

    def test_error_retry_and_draft_clear(self):
        composer = self.composer()
        self.page.route('**/api/assistant/workspace?*', lambda route: route.fulfill(status=500, body='failed'))
        composer.fill('///')
        expect(self.page.get_by_role('alert')).to_have_text('读取 workspace 失败，请重新输入 /// 重试')
        composer.press('Enter')
        expect(composer).to_have_value('///')
        self.page.unroute('**/api/assistant/workspace?*')
        composer.fill('')
        expect(self.page.get_by_role('listbox')).to_have_count(0)
        composer.fill('///')
        expect(self.page.get_by_role('option')).to_have_count(3)

    def test_mobile_selection(self):
        self.page.close()
        self.page = self.browser.new_page(viewport={'width': 390, 'height': 844}, has_touch=True)
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.page.route('**/api/**', self.api)
        composer = self.composer()
        composer.fill('///')
        self.page.get_by_role('option', name='app', exact=True).tap()
        expect(composer).to_have_value('/srv/workspace/app')
        self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'))

    def test_loading_enter_and_stale_response(self):
        composer = self.composer()
        pending = []

        def delay_root(route):
            if parse_qs(urlparse(route.request.url).query).get('path'):
                self.api(route)
            else:
                pending.append(route)

        self.page.route('**/api/assistant/workspace?*', delay_root)
        composer.fill('///')
        expect(self.page.locator('.directory-completion').get_by_role('status')).to_have_text('正在读取目录…')
        composer.press('Enter')
        expect(composer).to_have_value('///')
        composer.fill('///app/s')
        expect(self.page.get_by_role('option', name='src')).to_be_visible()
        self.assertEqual(len(pending), 1)
        self.api(pending[0])
        composer.press('Enter')
        expect(composer).to_have_value('/srv/workspace/app/src')


if __name__ == '__main__':
    unittest.main()
