"""Archive/restore browser regression with mocked APIs. Start Vite on port 5198."""
import json
import os
import re
import unittest
from urllib.parse import parse_qs, urlparse

from playwright.sync_api import expect, sync_playwright


class ConversationArchiveUI(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        playwright = sync_playwright().start()
        cls.addClassCleanup(playwright.stop)
        cls.browser = playwright.chromium.launch(headless=True)
        cls.addClassCleanup(cls.browser.close)

    def exercise(self, width, height):
        page = self.browser.new_page(viewport={'width': width, 'height': height})
        self.addCleanup(page.context.close)
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.add_init_script('window.WebSocket = class { readyState = 0; send() {} close() {} }')
        rows = [dict(id=1, title='准备归档的对话', agent='firefly', message_count=2,
                     status='completed', is_archived=False, last_message_id=10,
                     read_message_id=10, workspace_mode='shared', workspace_state='ready')]
        rows.append({**rows[0], 'id': 2, 'title': '保留的未归档对话'})
        rows.extend({**rows[0], 'id': 100 + index, 'title': f'已归档记录 {index}',
                     'is_archived': True, 'is_pinned': index == 0} for index in range(21))
        patches = []

        def api(route):
            parsed = urlparse(route.request.url)
            query = parse_qs(parsed.query)
            path = parsed.path
            data = {}
            if path == '/api/assistant/conversations':
                archived = query.get('archived', ['false'])[0] == 'true'
                selected = next((row for row in rows if str(row['id']) == query.get('selected_id', [''])[0]), None)
                matching = [row for row in rows if row['is_archived'] == archived]
                before = query.get('before_id', [''])[0]
                start = next((index + 1 for index, row in enumerate(matching) if str(row['id']) == before), 0)
                page_rows = matching[start:start + 20]
                data = {'conversations': page_rows, 'selected_conversation': selected,
                        'next_cursor': {'before_pinned': False, 'before_at': '2026-10-08T00:00:00Z',
                                        'before_id': page_rows[-1]['id']} if start + 20 < len(matching) else None}
            elif path == '/api/assistant/conversations/1' and route.request.method == 'PATCH':
                patch = route.request.post_data_json
                patches.append(patch)
                rows[0].update(patch)
                data = {'ok': True}
            elif path.endswith('/messages'):
                data = {'messages': [{'id': 10, 'conversation_id': 1, 'role': 'assistant',
                                     'content': '归档前保存的回复', 'attachments': []}], 'has_more': False}
            elif path.endswith('/active-generation'):
                data = {'active': False}
            route.fulfill(content_type='application/json', body=json.dumps(data))

        page.route('**/api/**', api)
        page.goto(os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5198/') + '?conversation=1')
        expect(page.get_by_text('归档前保存的回复', exact=True)).to_be_visible(timeout=15000)
        if width < 768:
            page.locator('.conversation-column').get_by_role('button', name='展开侧边栏', exact=True).click()
        sidebar = page.locator('.tool-panel')
        if width >= 768 and 'collapsed' in (sidebar.get_attribute('class') or '').split():
            sidebar.get_by_role('button', name='展开侧栏', exact=True).click()
        all_items = sidebar.locator('.conversation-item')
        item = all_items.filter(has_text='准备归档的对话')
        expect(all_items).to_have_count(2)
        expect(sidebar.locator('.conversation-view-switch')).to_have_count(0)
        expect(item).to_have_count(1)
        if width >= 768:
            item.hover()
        item.get_by_role('button', name='更多操作', exact=True).click()
        sidebar.get_by_role('menuitem', name='归档', exact=True).click()
        expect(item).to_have_count(0)
        expect(all_items).to_have_count(1)
        self.assertTrue(rows[0]['is_archived'])
        expect(page.locator('.archived-conversation-notice')).to_be_visible()
        archive_toggle = sidebar.get_by_role('button', name='查看归档对话', exact=True)
        expect(archive_toggle).to_have_attribute('aria-expanded', 'false')
        archive_toggle.click()
        expect(sidebar.get_by_role('button', name='收起归档对话', exact=True)).to_have_attribute('aria-expanded', 'true')
        expect(item).to_have_count(1)
        expect(all_items).to_have_count(21)
        expect(all_items.first).to_contain_text('保留的未归档对话')
        sidebar.get_by_role('button', name='查看更多对话', exact=True).click()
        expect(all_items).to_have_count(23)
        expect(sidebar.get_by_role('button', name='查看更多对话', exact=True)).to_have_count(0)
        if width < 768:
            item.click()
            expect(sidebar).not_to_have_class(re.compile(r'\bopen\b'))
        expect(page.get_by_text('归档前保存的回复', exact=True)).to_be_visible()
        page.get_by_role('button', name='取消归档并继续', exact=True).click()
        expect(page.locator('.archived-conversation-notice')).to_have_count(0)
        if width < 768:
            page.locator('.conversation-column').get_by_role('button', name='展开侧边栏', exact=True).click()
        expect(sidebar.locator('.conversation-view-switch')).to_have_count(0)
        expect(all_items.first).to_contain_text('准备归档的对话')
        expect(item).to_have_count(1)
        self.assertFalse(rows[0]['is_archived'])
        self.assertEqual(patches, [{'is_archived': True}, {'is_archived': False}])
        self.assertEqual(errors, [])

    def test_desktop_archive_and_restore(self):
        self.exercise(1280, 800)

    def test_mobile_archive_and_restore(self):
        self.exercise(390, 844)


if __name__ == '__main__':
    unittest.main()
