"""Mocked browser regression. Start Vite, then run with PARALLEL_TEST_URL.

python3 frontend/tests/e2e/test_parallel_workspace_ui.py
PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH may point to an existing browser.
"""
import json
import os
from urllib.parse import urlparse, parse_qs
import unittest
from playwright.sync_api import sync_playwright, expect


class ParallelWorkspaceUI(unittest.TestCase):
    def setUp(self):
        self.playwright = sync_playwright().start()
        self.addCleanup(self.playwright.stop)
        options = {'headless': True}
        if os.getenv('PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH'):
            options['executable_path'] = os.environ['PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH']
        self.browser = self.playwright.chromium.launch(**options)
        self.addCleanup(self.browser.close)
        self.page = self.browser.new_page()
        self.errors = []
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.conversations = []
        self.creations = []
        self.retry_count = 0
        self.page.route('**/api/**', self.api)

    def tearDown(self):
        self.assertEqual(self.errors, [])

    def conversation(self, state='ready'):
        return dict(id=1, title='并行任务', agent='codex', cwd='.assistant-parallel/test/work',
                    source_cwd='project', workspace_mode='parallel', workspace_state=state,
                    workspace_error='模拟复制失败' if state == 'failed' else None,
                    status='completed', message_count=1,
                    created_at='2026-09-27T01:00:00Z', updated_at='2026-09-27T01:00:00Z')

    def api(self, route):
        request = route.request
        url = urlparse(request.url)
        path = url.path
        result = {}
        if path.endswith('/conversations'):
            if request.method == 'POST':
                self.creations.append(request.post_data_json)
                result = self.conversation('pending')
                result['source_cwd'] = request.post_data_json.get('cwd', '')
                self.conversations = [result]
            else:
                result = {'conversations': self.conversations}
        elif path.endswith('/workspace'):
            directory = parse_qs(url.query).get('path', [''])[0]
            result = {'path': directory, 'entries': [] if directory else [{'name': 'project', 'path': 'project'}]}
        elif path.endswith('/codex/providers'):
            result = {'providers': [], 'official_models': ['test-model']}
        elif path.endswith('/workspace/retry'):
            self.retry_count += 1
            self.conversations[0].update(workspace_state='ready', workspace_error=None)
            result = {'ok': True}
        elif path.endswith('/active-generation'):
            result = {'active': False}
        elif '/jobs/' in path and path.endswith('/stream'):
            events = [('workspace_progress', {'conversation_id': 1, 'workspace_state': 'preparing', 'message': '复制工作目录'}),
                      ('workspace_progress', {'conversation_id': 1, 'workspace_state': 'ready', 'cwd': '.assistant-parallel/test/work'}),
                      ('assistant_done', {'done': True})]
            self.conversations[0]['workspace_state'] = 'ready'
            route.fulfill(content_type='text/event-stream', body=''.join(
                f'event: {event}\ndata: {json.dumps(dict(payload, seq=index + 1))}\n\n'
                for index, (event, payload) in enumerate(events)))
            return
        elif path.endswith('/firefly/messages'):
            result = {'turns': [], 'has_more': False}
        elif path.endswith('/messages'):
            message = dict(id=10, conversation_id=1, role='user', content='开始工作',
                           created_at='2026-09-27T01:00:00Z', attachments=[], generation_job_id='test-job')
            result = {'job_id': 'test-job', 'status': 'running', 'message': message} if request.method == 'POST' else {'messages': [message], 'has_more': False}
        elif path.endswith('/files'):
            result = {'entries': [], 'path': ''}
        elif path.endswith('/process'):
            result = {'messages': []}
        route.fulfill(content_type='application/json', body=json.dumps(result))

    def open(self, conversation=False):
        base = os.getenv('PARALLEL_TEST_URL', 'http://127.0.0.1:5187/')
        self.page.goto(base + ('?conversation=1' if conversation else ''))
        expect(self.page.get_by_role('button', name='新对话', exact=True)).to_be_visible()

    def test_lazy_creation_sends_parallel_mode_and_stream_updates_readiness(self):
        self.open()
        self.page.get_by_role('button', name='新建工作', exact=True).click()
        dialog = self.page.get_by_role('dialog', name='新建工作')
        expect(dialog.locator('input[type=checkbox]')).not_to_be_checked()
        dialog.get_by_role('button', name='进入目录 project', exact=True).click()
        dialog.locator('input[type=checkbox]').check()
        dialog.get_by_role('button', name='开始工作', exact=True).click()
        self.assertEqual(self.creations, [])
        expect(self.page.locator('.parallel-status')).to_contain_text('首次发送')
        expect(self.page.get_by_role('button', name='文件', exact=True)).to_be_disabled()
        self.page.locator('.conversation-column textarea.chat-input').fill('开始工作')
        self.page.get_by_role('button', name='发送消息', exact=True).click()
        expect(self.page.locator('.parallel-status')).to_contain_text('独立工作目录已就绪')
        expect(self.page.get_by_role('button', name='文件', exact=True)).to_be_enabled()
        self.assertEqual(self.creations[0]['workspace_mode'], 'parallel')
        self.assertEqual(self.creations[0]['cwd'], 'project')

    def test_failed_workspace_retry_and_group_source(self):
        self.conversations = [self.conversation('failed')]
        self.open(conversation=True)
        expect(self.page.get_by_role('button', name='文件', exact=True)).to_be_disabled()
        expect(self.page.locator('.conversation-column textarea.chat-input')).to_be_disabled()
        self.page.get_by_role('button', name='重试目录准备', exact=True).click()
        expect(self.page.locator('.parallel-status')).to_contain_text('独立工作目录已就绪')
        self.assertEqual(self.retry_count, 1)
        self.page.locator('.workspace-header').hover()
        self.page.get_by_role('button', name='在 project 中新建工作', exact=True).click()
        dialog = self.page.get_by_role('dialog', name='新建工作')
        expect(dialog.get_by_role('navigation', name='当前工作目录').locator('[aria-current=location]')).to_have_text('project')
        expect(dialog.locator('input[type=checkbox]')).to_be_checked()

    def test_new_work_navigation_and_keyboard_focus(self):
        self.page.set_viewport_size({'width': 390, 'height': 844})
        self.open()
        self.page.get_by_role('button', name='展开侧边栏', exact=True).click()
        trigger = self.page.get_by_role('button', name='新建工作', exact=True)
        trigger.click()
        dialog = self.page.get_by_role('dialog', name='新建工作')
        dialog.get_by_role('button', name='进入目录 project', exact=True).click()
        expect(dialog.get_by_role('navigation', name='当前工作目录').locator('[aria-current=location]')).to_have_text('project')
        expect(dialog.get_by_text('此目录下还没有文件夹', exact=True)).to_be_visible()
        dialog.get_by_role('navigation').get_by_role('button', name='workspace', exact=True).click()
        expect(dialog.get_by_role('button', name='进入目录 project', exact=True)).to_be_visible()
        expect(dialog.get_by_role('navigation', name='当前工作目录').locator('[aria-current=location]')).to_have_text('workspace')
        dialog.get_by_role('button', name='新建目录', exact=True).click()
        expect(dialog.get_by_role('textbox', name='新目录名称')).to_be_focused()
        self.page.keyboard.press('Escape')
        expect(dialog).to_be_visible()
        expect(dialog.get_by_role('textbox', name='新目录名称')).to_have_count(0)
        dialog.get_by_role('button', name='开始工作', exact=True).focus()
        self.page.keyboard.press('Tab')
        expect(dialog.get_by_role('button', name='关闭', exact=True)).to_be_focused()
        self.page.keyboard.press('Shift+Tab')
        expect(dialog.get_by_role('button', name='开始工作', exact=True)).to_be_focused()
        self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'))
        self.page.keyboard.press('Escape')
        expect(dialog).to_have_count(0)
        expect(trigger).to_be_focused()

    def test_mobile_paths_and_preparing_state_survive_reload(self):
        self.page.set_viewport_size({'width': 390, 'height': 844})
        self.conversations = [self.conversation('preparing')]
        base = os.getenv('PARALLEL_TEST_URL', 'http://127.0.0.1:5187/')
        self.page.goto(base + '?conversation=1')
        expect(self.page.locator('.parallel-status')).to_contain_text('正在准备')
        expect(self.page.get_by_role('button', name='文件', exact=True)).to_be_disabled()
        self.page.reload()
        expect(self.page.locator('.parallel-status')).to_contain_text('正在准备')
        self.page.locator('.parallel-status summary').click()
        expect(self.page.locator('.parallel-status')).to_contain_text('.assistant-parallel/test/work')
        self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'))

    def test_mobile_header_stays_on_one_row_and_exposes_full_context(self):
        conversation = self.conversation('ready')
        conversation.update(title='当前项目里还有哪些功能，查看完整代码实现并优化移动端标题栏' * 2,
                            codex_provider='openai', codex_model='gpt-6.1-sol',
                            cwd='project/very-long-workspace-name/nested/frontend')
        self.conversations = [conversation]
        self.page.set_viewport_size({'width': 390, 'height': 844})
        self.open(conversation=True)
        toolbar = self.page.locator('.conversation-toolbar')
        trigger = toolbar.get_by_role('button', name='查看对话信息', exact=True)
        card = toolbar.get_by_role('region', name='对话信息', exact=True)
        for width in [320, 390, 768]:
            self.page.set_viewport_size({'width': width, 'height': 844})
            expect(trigger).to_be_visible()
            self.assertLessEqual(toolbar.bounding_box()['height'], 64)
            self.assertLessEqual(trigger.locator('.toolbar-title').bounding_box()['height'], 24)
            self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'))
            expect(toolbar.get_by_role('button', name='文件', exact=True)).to_be_visible()
            trigger.click()
            expect(card).to_be_visible()
            expect(card).to_contain_text(conversation['title'])
            expect(card).to_contain_text('Codex · openai · gpt-6.1-sol')
            expect(card).to_contain_text(conversation['cwd'])
            box = card.bounding_box()
            self.assertGreaterEqual(box['x'], 0)
            self.assertLessEqual(box['x'] + box['width'], width)
            self.assertLessEqual(toolbar.bounding_box()['height'], 64)
            self.page.keyboard.press('Escape')
            expect(card).not_to_be_visible()
            expect(trigger).to_be_focused()
        trigger.focus()
        self.page.keyboard.press('Enter')
        expect(card).to_be_visible()
        self.page.keyboard.press('Tab')
        expect(card).to_be_focused()
        self.page.keyboard.press('Tab')
        expect(toolbar.get_by_role('button', name='文件', exact=True)).to_be_focused()
        expect(card).not_to_be_visible()
        trigger.click()
        self.page.locator('.chat-panel').click(position={'x': 10, 'y': 400})
        expect(card).not_to_be_visible()
        trigger.click()
        self.page.set_viewport_size({'width': 1280, 'height': 844})
        expect(trigger).not_to_be_visible()
        expect(card).not_to_be_visible()
        expect(toolbar.locator('.toolbar-context-main .toolbar-title')).to_have_attribute('title', conversation['title'])
        expect(toolbar.locator('.toolbar-agent')).to_have_text('Codex · openai · gpt-6.1-sol')

    def test_codex_quick_phrases_and_scroll_bottom_remain_functional(self):
        self.conversations = [self.conversation('ready')]
        phrases = ['继续', '提交并推送', '查看目前实现的功能并继续优化' * 8]
        self.page.route('**/api/assistant/quick-phrases', lambda route: route.fulfill(json={'phrases': phrases}))
        self.page.route('**/conversations/1/messages*', lambda route: route.fulfill(json={
            'messages': [dict(id=10, conversation_id=1, role='assistant', attachments=[],
                              content='\n\n'.join(f'第 {index} 段历史回复内容。' for index in range(80)),
                              created_at='2026-09-27T01:00:00Z')], 'has_more': False
        }))
        self.page.set_viewport_size({'width': 390, 'height': 844})
        self.open(conversation=True)
        column = self.page.locator('.conversation-column')
        chips = column.get_by_role('group', name='常用语', exact=True)
        expect(chips.get_by_role('button')).to_have_count(len(phrases))
        chips.get_by_role('button', name='继续', exact=True).click()
        chips.get_by_role('button', name='提交并推送', exact=True).click()
        expect(column.get_by_role('textbox', name='消息', exact=True)).to_have_value('继续\n提交并推送')
        column.locator('.chat-panel').evaluate("el => { el.scrollTo({top: 0, behavior: 'instant'}); el.dispatchEvent(new Event('scroll')); }")
        jump = column.get_by_role('button', name='回到底部', exact=True)
        expect(jump).to_be_visible()
        expect(jump.locator('svg')).to_be_visible()
        jump.click()
        expect(jump).to_have_count(0)
        self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'))

    def test_conversation_reload_and_load_more_keep_complete_turns(self):
        self.conversations = [self.conversation('ready')]
        calls = []

        def record(id_, role, content, job):
            return dict(id=id_, conversation_id=1, role=role, content=content,
                        generation_job_id=job, processing_status='completed',
                        created_at='2026-09-27T01:00:00Z', attachments=[])

        def summary(id_, end, job):
            return dict(id=id_, conversation_id=1, role='process', generation_job_id=job,
                        status='completed', start_id=id_, end_id=end,
                        created_at='2026-09-27T01:00:00Z', started_at='2026-09-27T01:00:00Z',
                        completed_at='2026-09-27T01:18:07Z')

        latest = [record(10, 'user', '优化标题栏', 'job-a'), summary(11, 20, 'job-a'),
                  record(21, 'assistant', '标题栏已优化', 'job-a'),
                  record(22, 'user', '优化按钮', 'job-b'), summary(23, 30, 'job-b'),
                  record(31, 'assistant', '按钮已优化', 'job-b')]
        older = [record(1, 'user', '更早的任务', 'job-old'),
                 record(2, 'assistant', '更早的结果', 'job-old')]

        def messages(route):
            args = parse_qs(urlparse(route.request.url).query)
            calls.append(args)
            is_older = args.get('before') == ['10']
            route.fulfill(json=dict(messages=older if is_older else latest,
                                    has_more=not is_older, next_before=None if is_older else 10))

        self.page.route('**/conversations/1/messages*', messages)
        self.open(conversation=True)
        column = self.page.locator('.conversation-column')
        for width in [1280, 390]:
            self.page.set_viewport_size({'width': width, 'height': 844})
            self.page.reload()
            expect(column.locator('.message.user .plain-text')).to_have_text(['优化标题栏', '优化按钮'])
            expect(column.locator('.message.assistant .markdown-body')).to_have_text(['标题栏已优化', '按钮已优化'])
            expect(column.locator('.process-group')).to_have_count(2)
            expect(column.locator('.process-group-body')).to_have_count(0)
            expect(column.get_by_text('更早的任务', exact=True)).to_have_count(0)
            self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'))
        column.get_by_role('button', name='查看更多记录', exact=True).click()
        expect(column.locator('.message.user .plain-text')).to_have_text(['更早的任务', '优化标题栏', '优化按钮'])
        expect(column.locator('.message.assistant .markdown-body')).to_have_text(['更早的结果', '标题栏已优化', '按钮已优化'])
        expect(column.locator('.process-group')).to_have_count(2)
        expect(column.get_by_role('button', name='查看更多记录', exact=True)).to_have_count(0)
        self.assertEqual(sum(args.get('before') == ['10'] for args in calls), 1)


if __name__ == '__main__':
    unittest.main()
