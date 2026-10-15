"""Mocked browser regression and captures for the new conversation page.

Start Vite on 5187. All assistant APIs and WebSockets are mocked, so tests never
invoke a model or modify live conversations. FIREFLY_TEST_URL overrides the URL.
FIREFLY_TEST_BROWSER selects chromium or webkit; FIREFLY_TEST_BROWSER_PATH can
point to an existing browser. Captures are saved under output/new-chat-ui/.
"""
import json
import os
import re
from pathlib import Path
from urllib.parse import parse_qs, urlparse
import unittest

from playwright.sync_api import expect, sync_playwright


class NewConversationUI(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        playwright = sync_playwright().start()
        cls.addClassCleanup(playwright.stop)
        engine = getattr(playwright, os.getenv('FIREFLY_TEST_BROWSER', 'chromium'))
        options = {'headless': True}
        if os.getenv('FIREFLY_TEST_BROWSER_PATH'):
            options['executable_path'] = os.environ['FIREFLY_TEST_BROWSER_PATH']
        cls.browser = engine.launch(**options)
        cls.addClassCleanup(cls.browser.close)

    def setUp(self):
        context = self.browser.new_context(viewport={'width': 390, 'height': 844},
                                           is_mobile=True, has_touch=True)
        self.page = context.new_page()
        self.addCleanup(self.page.context.close)
        self.errors = []
        self.creations = []
        self.deletions = []
        self.renames = []
        self.pins = []
        self.archive_requests = 0
        self.codex_updates = []
        self.claude_updates = []
        self.codex_save_failure = False
        self.conversations = [dict(id=1, title='查询示例资料', agent='firefly', message_count=2),
                              dict(id=2, title='移动端排版优化', agent='codex', message_count=2),
                              dict(id=3, title='新对话页面设计', agent='firefly', message_count=2)]
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.page.add_init_script('''window.WebSocket = class {
          constructor() { this.readyState = 0; setTimeout(() => { this.readyState = 1; this.onopen?.() }, 0) }
          send() {} close() { this.readyState = 3 }
        };''')
        self.page.route('**/api/**', self.api)

    def tearDown(self):
        self.assertEqual(self.errors, [])

    def api(self, route):
        request = route.request
        url = urlparse(request.url)
        path = url.path
        data = {}
        if path.endswith('/conversations'):
            if request.method == 'POST':
                fields = request.post_data_json
                self.creations.append(fields)
                data = dict(fields, id=10, message_count=1, status='completed')
                self.conversations.insert(0, data)
            else:
                params = parse_qs(url.query)
                query = params.get('search', [''])[0].strip().lower()
                archived = params.get('archived', ['false'])[0] == 'true'
                self.archive_requests += int(archived)
                matching = [item for item in self.conversations if query in
                            ' '.join(str(item.get(key, '')) for key in ('title', 'cwd', 'source_cwd', 'agent')).lower()
                            and bool(item.get('is_archived')) == archived]
                data = dict(conversations=sorted(matching, key=lambda item: bool(item.get('is_pinned')), reverse=True))
                selected_id = params.get('selected_id', [''])[0]
                if selected_id:
                    data['selected_conversation'] = next((item for item in self.conversations if str(item['id']) == selected_id), None)
        elif path.endswith('/codex-settings'):
            conversation_id = int(path.split('/')[-2])
            self.codex_updates.append(request.post_data_json)
            if self.codex_save_failure:
                route.fulfill(status=503, content_type='application/json',
                              body=json.dumps({'error': '保存模型设置失败，请稍后重试'}))
                return
            item = next(item for item in self.conversations if item['id'] == conversation_id)
            item.update(codex_provider=request.post_data_json['provider'],
                        codex_model=request.post_data_json['model'],
                        codex_reasoning_level=request.post_data_json['reasoning_level'])
            data = item
        elif path.endswith('/claude-settings'):
            conversation_id = int(path.split('/')[-2])
            fields = request.post_data_json
            self.claude_updates.append(fields)
            item = next(item for item in self.conversations if item['id'] == conversation_id)
            item.update(claude_provider=fields['provider'], claude_model=fields['model'],
                        claude_reasoning_level=fields['reasoning_level'])
            data = item
        elif re.fullmatch(r'/api/assistant/conversations/\d+', path):
            conversation_id = int(path.rsplit('/', 1)[1])
            if request.method == 'DELETE':
                self.deletions.append(conversation_id)
                self.conversations = [item for item in self.conversations if item['id'] != conversation_id]
            elif request.method == 'PATCH':
                fields = request.post_data_json
                if 'is_pinned' in fields:
                    self.pins.append(fields)
                else:
                    self.renames.append(fields)
                item = next(item for item in self.conversations if item['id'] == conversation_id)
                item.update(fields)
                data = item
        elif path.endswith('/workspace'):
            directory = parse_qs(url.query).get('path', [''])[0]
            data = dict(path=directory, entries=[] if directory else [
                dict(name='firefly', path='projects/firefly'),
                dict(name='很长的工作目录名称用来检查移动端省略显示', path='projects/很长的工作目录名称用来检查移动端省略显示')])
        elif path.endswith('/codex/providers'):
            data = dict(default='openai', default_model='test-model', default_reasoning_level='medium',
                        providers=[], official_models=['test-model'], model_reasoning=[
                            dict(model='test-model', reasoning_levels=['low', 'medium', 'high'], default_reasoning_level='medium')])
        elif path.endswith('/claude/models'):
            data = dict(default='cli', cli_default_model='sonnet', default_model='sonnet',
                        models=['sonnet', 'opus', 'haiku'], reasoning_levels=['low', 'medium', 'high', 'xhigh', 'max'],
                        providers=[dict(name='relay', models=['custom-sonnet', 'custom-opus'], default_model='custom-sonnet')])
        elif path.endswith('/active-generation'):
            data = {'active': False}
        elif path.endswith('/messages'):
            message = dict(id=100, conversation_id=10, role='user', content='帮我优化移动端', attachments=[])
            data = dict(status='completed', message=message) if request.method == 'POST' else dict(messages=[], has_more=False)
        elif path.endswith('/ai-rankings'):
            data = {'available': False, 'message': '测试数据源暂时不可用'}
        route.fulfill(content_type='application/json', body=json.dumps(data))

    def open_page(self):
        self.page.goto(os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5187/'))
        expect(self.page.get_by_role('heading', name='想聊点什么？')).to_be_visible(timeout=15000)
        expect(self.page.get_by_role('button', name='选择 Agent', exact=True)).to_be_enabled()
        expect(self.page.get_by_role('dialog')).to_have_count(0)

    def choose_agent(self, name):
        self.page.get_by_role('button', name='选择 Agent', exact=True).click()
        self.page.get_by_role('menuitemradio', name=name).click()
        expect(self.page.get_by_role('button', name='选择 Agent', exact=True)).to_contain_text(name)
        if self.page.locator('textarea').input_value():
            expect(self.page.get_by_role('button', name='发送消息', exact=True)).to_be_enabled()

    def test_existing_codex_settings_save_retry_focus_and_mobile_layout(self):
        self.conversations[1].update(cwd='projects/firefly', codex_provider='openai',
                                     codex_model='test-model', codex_reasoning_level='high', status='completed')
        self.open_page()
        self.page.get_by_role('button', name='展开侧边栏', exact=True).click()
        self.page.locator('.conversation-item').filter(has_text='移动端排版优化').click()
        trigger = self.page.get_by_role('button', name='模型设置', exact=True)
        expect(trigger).to_be_enabled()
        composer = self.page.get_by_role('textbox', name='消息', exact=True)
        composer.fill('继续使用原会话的草稿')
        trigger.click()
        dialog = self.page.get_by_role('dialog', name='模型设置', exact=True)
        expect(dialog).to_be_visible()
        expect(dialog.get_by_role('combobox', name='模型', exact=True)).to_have_value('test-model')
        expect(dialog.get_by_role('combobox', name='推理级别', exact=True)).to_have_value('high')
        for width in [320, 390, 1280]:
            self.page.set_viewport_size(dict(width=width, height=844))
            bounds = dialog.bounding_box()
            self.assertGreaterEqual(bounds['x'], 0)
            self.assertLessEqual(bounds['x'] + bounds['width'], width)
            self.assertLessEqual(bounds['y'] + bounds['height'], 844)
        dialog.get_by_role('button', name='保存', exact=True).focus()
        self.page.keyboard.press('Tab')
        expect(dialog.get_by_role('button', name='关闭模型设置', exact=True)).to_be_focused()
        self.page.keyboard.press('Shift+Tab')
        expect(dialog.get_by_role('button', name='保存', exact=True)).to_be_focused()
        self.page.keyboard.press('Escape')
        expect(dialog).to_have_count(0)
        expect(trigger).to_be_focused()
        trigger.click()
        dialog.get_by_role('combobox', name='推理级别', exact=True).select_option('low')
        self.codex_save_failure = True
        dialog.get_by_role('button', name='保存', exact=True).click()
        expect(dialog.get_by_role('alert')).to_contain_text('保存模型设置失败')
        expect(dialog.get_by_role('combobox', name='推理级别', exact=True)).to_have_value('low')
        self.codex_save_failure = False
        dialog.get_by_role('button', name='保存', exact=True).click()
        expect(dialog).to_have_count(0)
        expect(composer).to_have_value('继续使用原会话的草稿')
        self.assertIn('conversation=2', self.page.url)
        self.assertEqual(self.creations, [])
        self.assertEqual(self.codex_updates[-1], dict(provider='openai', model='test-model', reasoning_level='low'))
        self.page.reload()
        trigger.click()
        expect(dialog.get_by_role('combobox', name='推理级别', exact=True)).to_have_value('low')
        captures = Path('output/codex-settings-ui')
        captures.mkdir(parents=True, exist_ok=True)
        self.page.screenshot(path=str(captures / 'desktop.png'))
        self.page.set_viewport_size(dict(width=390, height=844))
        self.page.screenshot(path=str(captures / 'mobile.png'))

    def test_claude_platform_model_and_effort_in_new_and_existing_conversations(self):
        self.conversations[1].update(agent='claude', claude_provider='relay', claude_model='custom-opus',
                                     claude_reasoning_level='max', status='running')
        self.open_page()
        self.choose_agent('Claude')
        self.page.get_by_role('button', name='配置 Agent', exact=True).click()
        settings = self.page.get_by_role('group', name='Claude 配置', exact=True)
        expect(settings.get_by_role('combobox', name='API 平台', exact=True)).to_be_visible()
        settings.get_by_role('combobox', name='API 平台', exact=True).select_option('relay')
        settings.get_by_role('combobox', name='模型', exact=True).select_option('custom-opus')
        settings.get_by_role('combobox', name='推理级别', exact=True).select_option('high')
        for width in [320, 390, 1280]:
            self.page.set_viewport_size(dict(width=width, height=844))
            self.assertFalse(self.page.evaluate('document.documentElement.scrollWidth > innerWidth'))
        self.page.get_by_role('textbox', name='消息', exact=True).fill('测试 Claude 平台配置')
        self.page.get_by_role('button', name='发送消息', exact=True).click()
        self.page.wait_for_function('location.search.includes("conversation=10")')
        self.assertEqual(self.creations[-1]['provider'], 'relay')
        self.assertEqual(self.creations[-1]['model'], 'custom-opus')
        self.assertEqual(self.creations[-1]['reasoning_level'], 'high')
        self.page.goto(os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5187/') + '?conversation=2')
        self.page.get_by_role('button', name='模型设置', exact=True).click()
        dialog = self.page.get_by_role('dialog', name='模型设置', exact=True)
        expect(dialog.get_by_role('combobox', name='API 平台', exact=True)).to_have_value('relay')
        expect(dialog.get_by_role('combobox', name='模型', exact=True)).to_have_value('custom-opus')
        expect(dialog.get_by_role('combobox', name='推理级别', exact=True)).to_have_value('max')
        captures = Path('output/claude-settings-ui')
        captures.mkdir(parents=True, exist_ok=True)
        for width, name in [(1280, 'desktop'), (390, 'mobile'), (320, 'narrow')]:
            self.page.set_viewport_size(dict(width=width, height=844))
            bounds = dialog.bounding_box()
            self.assertGreaterEqual(bounds['x'], 0)
            self.assertLessEqual(bounds['x'] + bounds['width'], width)
            self.page.screenshot(path=str(captures / f'{name}.png'))
        dialog.get_by_role('combobox', name='API 平台', exact=True).select_option('cli')
        expect(dialog.get_by_role('combobox', name='模型', exact=True)).to_have_value('')
        dialog.get_by_role('combobox', name='模型', exact=True).select_option('opus')
        dialog.get_by_role('combobox', name='推理级别', exact=True).select_option('low')
        dialog.get_by_role('button', name='保存', exact=True).click()
        expect(dialog).to_have_count(0)
        self.assertEqual(self.claude_updates[-1], dict(provider='cli', model='opus', reasoning_level='low'))
        self.page.reload()
        self.page.get_by_role('button', name='模型设置', exact=True).click()
        expect(dialog.get_by_role('combobox', name='API 平台', exact=True)).to_have_value('cli')
        expect(dialog.get_by_role('combobox', name='推理级别', exact=True)).to_have_value('low')

    def test_codex_settings_allow_busy_and_disable_archived_conversations(self):
        self.open_page()
        for patch in [dict(status='running'), dict(status='queued')]:
            self.conversations[1].update(patch)
            self.page.goto(os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5187/') + '?conversation=2')
            trigger = self.page.get_by_role('button', name='模型设置', exact=True)
            expect(trigger).to_be_enabled()
            trigger.click()
            dialog = self.page.get_by_role('dialog', name='模型设置', exact=True)
            expect(dialog.get_by_role('button', name='保存', exact=True)).to_be_enabled()
            expect(dialog).to_contain_text('运行和排队的消息保持原设置')
            dialog.get_by_role('button', name='保存', exact=True).click()
            expect(dialog).to_have_count(0)
        self.conversations[1].update(status='completed', is_archived=True)
        self.page.goto(os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5187/') + '?conversation=2')
        expect(self.page.get_by_role('button', name='模型设置', exact=True)).to_be_disabled()

    def menu(self):
        self.page.get_by_role('button', name='更多功能', exact=True).click()
        menu = self.page.get_by_role('menu', name='更多功能', exact=True)
        expect(menu).to_be_visible()
        return menu

    def test_new_page_switch_preserves_text_and_attachments(self):
        self.open_page()
        create = self.page.get_by_role('button', name='新对话', exact=True)
        expect(create).to_have_count(1)
        expect(create).to_be_visible()
        self.assertEqual(create.inner_text(), '')
        expect(create.locator('svg')).to_have_count(1)
        toggle = self.page.get_by_role('button', name='展开侧边栏', exact=True)
        create_box, toggle_box = create.bounding_box(), toggle.bounding_box()
        self.assertGreaterEqual(create_box['x'], toggle_box['x'] + toggle_box['width'])
        self.assertEqual(create_box['y'], toggle_box['y'])
        composer = self.page.get_by_role('textbox', name='消息', exact=True)
        composer.fill('保留这段草稿')
        self.page.locator('.input-box input[type=file]').set_input_files(dict(name='draft.txt', mimeType='text/plain', buffer=b'draft'))
        self.choose_agent('Codex')
        expect(composer).to_have_value('保留这段草稿')
        expect(self.page.locator('.attachment-preview-name')).to_have_text('draft.txt')
        expect(self.page.get_by_role('button', name='选择工作目录', exact=True)).to_contain_text('workspace')
        self.choose_agent('Claude')
        expect(composer).to_have_value('保留这段草稿')
        self.choose_agent('流萤')
        expect(composer).to_have_value('保留这段草稿')
        expect(self.page.get_by_role('button', name='选择工作目录', exact=True)).to_have_count(0)
        self.assertEqual(self.creations, [])
        toggle.click()
        sidebar = self.page.get_by_role('complementary', name='工作区导航')
        expect(sidebar.get_by_role('button', name='新对话', exact=True)).to_have_count(0)
        sidebar.locator('.conversation-item').filter(has_text='移动端排版优化').click()
        expect(self.page.locator('.new-conversation-title')).to_have_count(0)
        create.click()
        expect(self.page.get_by_role('heading', name='想聊点什么？')).to_be_visible()
        expect(composer).to_have_value('')
        expect(self.page.locator('.attachment-preview-name')).to_have_count(0)
        self.assertNotIn('conversation=', self.page.url)
        self.assertEqual(self.creations, [])
        create.focus()
        create.press('Enter')
        expect(composer).to_have_value('')
        self.assertEqual(self.creations, [])

    def test_confirm_directory_config_and_first_send(self):
        self.open_page()
        self.choose_agent('Codex')
        self.page.get_by_role('button', name='选择工作目录', exact=True).click()
        picker = self.page.get_by_role('group', name='选择工作目录', exact=True)
        expect(self.page.get_by_role('dialog')).to_have_count(0)
        picker.get_by_role('button', name='进入目录 firefly', exact=True).click()
        picker.get_by_role('button', name='使用此目录', exact=True).click()
        expect(self.page.get_by_role('button', name='选择工作目录', exact=True)).to_contain_text('firefly')
        self.page.get_by_role('button', name=re.compile(r'^(配置 Agent|收起 Agent 配置)$')).click()
        settings = self.page.get_by_role('group', name='Codex 配置', exact=True)
        settings.get_by_label('推理级别', exact=True).select_option('high')
        settings.get_by_role('switch', name='并行模式', exact=True).check()
        self.page.get_by_role('button', name=re.compile(r'^(配置 Agent|收起 Agent 配置)$')).click()
        self.assertEqual(self.creations, [])
        self.page.get_by_role('textbox', name='消息', exact=True).fill('帮我优化移动端')
        self.page.get_by_role('button', name='发送消息', exact=True).click()
        expect(self.page.locator('.new-conversation-controls')).to_have_count(0)
        self.assertEqual(len(self.creations), 1)
        self.assertEqual(self.creations[0]['agent'], 'codex')
        self.assertEqual(self.creations[0]['cwd'], 'projects/firefly')
        self.assertEqual(self.creations[0]['reasoning_level'], 'high')
        self.assertEqual(self.creations[0]['workspace_mode'], 'parallel')

    def test_inline_config_preserves_draft_and_shrinks_for_claude(self):
        self.open_page()
        self.choose_agent('Codex')
        composer = self.page.get_by_role('textbox', name='消息', exact=True)
        composer.fill('保留配置调整时的草稿')
        self.page.locator('.input-box input[type=file]').set_input_files(dict(name='draft.txt', mimeType='text/plain', buffer=b'draft'))
        trigger = self.page.get_by_role('button', name=re.compile(r'^(配置 Agent|收起 Agent 配置)$'))
        trigger.click()
        settings = self.page.get_by_role('group', name='Codex 配置', exact=True)
        expect(settings).to_be_visible()
        expect(self.page.get_by_role('dialog')).to_have_count(0)
        expect(trigger).to_have_attribute('aria-expanded', 'true')
        settings.get_by_label('推理级别', exact=True).select_option('high')
        settings.get_by_role('switch', name='并行模式', exact=True).check()
        for width, height in [(320, 720), (390, 844), (768, 1024), (1440, 1024), (390, 400)]:
            self.page.set_viewport_size(dict(width=width, height=height))
            self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'))
            box = composer.bounding_box()
            self.assertGreaterEqual(box['y'], 0)
            self.assertLessEqual(box['y'] + box['height'], height)
        self.page.set_viewport_size(dict(width=390, height=844))
        trigger.click()
        expect(settings).to_have_count(0)
        trigger.click()
        expect(settings.get_by_label('推理级别', exact=True)).to_have_value('high')
        expect(settings.get_by_role('switch', name='并行模式', exact=True)).to_be_checked()
        settings.get_by_role('switch', name='并行模式', exact=True).focus()
        self.page.keyboard.press('Escape')
        expect(settings).to_have_count(0)
        expect(trigger).to_be_focused()
        self.choose_agent('Claude')
        trigger.click()
        claude = self.page.get_by_role('group', name='Claude 配置', exact=True)
        expect(claude.get_by_role('combobox')).to_have_count(0)
        expect(claude.get_by_role('switch', name='并行模式', exact=True)).to_be_visible()
        expect(composer).to_have_value('保留配置调整时的草稿')
        expect(self.page.locator('.attachment-preview-name')).to_have_text('draft.txt')
        self.assertEqual(self.creations, [])

    def test_inline_workspace_navigation_collapse_and_keyboard(self):
        self.open_page()
        self.choose_agent('Claude')
        composer = self.page.get_by_role('textbox', name='消息', exact=True)
        composer.fill('保留工作目录选择时的草稿')
        trigger = self.page.get_by_role('button', name='选择工作目录', exact=True)
        trigger.click()
        picker = self.page.get_by_role('group', name='选择工作目录', exact=True)
        expect(picker).to_be_visible()
        expect(trigger).to_have_attribute('aria-expanded', 'true')
        expect(self.page.get_by_role('dialog')).to_have_count(0)
        for width, height in [(320, 568), (390, 844), (768, 1024), (1440, 1024), (390, 400)]:
            self.page.set_viewport_size(dict(width=width, height=height))
            self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'))
            box = composer.bounding_box()
            self.assertGreaterEqual(box['y'], 0)
            self.assertLessEqual(box['y'] + box['height'], height)
        self.page.set_viewport_size(dict(width=390, height=844))
        picker.get_by_role('button', name='使用此目录', exact=True).focus()
        self.page.keyboard.press('Tab')
        expect(self.page.get_by_role('button', name='选择 Agent', exact=True)).to_be_focused()
        picker.get_by_role('button', name='进入目录 firefly', exact=True).click()
        expect(trigger).to_contain_text('workspace')
        picker.get_by_role('button', name='新建目录', exact=True).click()
        expect(picker.get_by_role('textbox', name='新目录名称', exact=True)).to_be_focused()
        self.page.keyboard.press('Escape')
        expect(picker).to_be_visible()
        expect(picker.get_by_role('textbox', name='新目录名称', exact=True)).to_have_count(0)
        self.page.keyboard.press('Escape')
        expect(picker).to_have_count(0)
        expect(trigger).to_be_focused()
        trigger.click()
        expect(picker.get_by_role('navigation').locator('[aria-current=location]')).to_have_text('workspace')
        picker.get_by_role('button', name='进入目录 firefly', exact=True).click()
        picker.get_by_role('button', name='使用此目录', exact=True).click()
        expect(trigger).to_have_attribute('title', 'workspace/projects/firefly')
        trigger.click()
        picker.get_by_role('button', name='上一级', exact=True).click()
        trigger.click()
        expect(picker).to_have_count(0)
        trigger.click()
        expect(picker.get_by_role('navigation').locator('[aria-current=location]')).to_have_text('firefly')
        picker.get_by_role('button', name='取消', exact=True).click()
        expect(composer).to_have_value('保留工作目录选择时的草稿')
        self.assertEqual(self.creations, [])

    def test_flat_menu_all_tools_keyboard_and_focus_restore(self):
        self.open_page()
        menu = self.menu()
        expect(menu.get_by_role('menuitem')).to_have_count(1)
        self.assertEqual([text.strip() for text in menu.get_by_role('menuitem').all_text_contents()],
                         ['定时任务'])
        expect(menu.get_by_role('menuitem', name='更多', exact=True)).to_have_count(0)
        self.page.keyboard.press('End')
        expect(menu.get_by_role('menuitem', name='定时任务', exact=True)).to_be_focused()
        self.page.keyboard.press('Escape')
        trigger = self.page.get_by_role('button', name='更多功能', exact=True)
        expect(trigger).to_be_focused()
        self.menu().get_by_role('menuitem', name='定时任务', exact=True).click()
        dialog = self.page.get_by_role('dialog', name='定时任务', exact=True)
        expect(dialog).to_be_visible()
        dialog.get_by_role('button', name='关闭定时任务', exact=True).click()
        expect(trigger).to_be_focused()

    def test_responsive_controls_menu_and_long_directory(self):
        self.open_page()
        self.choose_agent('Codex')
        self.page.get_by_role('button', name='选择工作目录', exact=True).click()
        picker = self.page.get_by_role('group', name='选择工作目录', exact=True)
        picker.get_by_role('button', name='进入目录 很长的工作目录名称用来检查移动端省略显示', exact=True).click()
        picker.get_by_role('button', name='使用此目录', exact=True).click()
        captures = Path('output/new-conversation-button')
        captures.mkdir(parents=True, exist_ok=True)
        for width, height in [(320, 568), (390, 844), (768, 844), (1440, 1024)]:
            self.page.set_viewport_size(dict(width=width, height=height))
            create = self.page.get_by_role('button', name='新对话', exact=True)
            expect(create).to_have_count(1)
            expect(create).to_be_visible()
            self.assertEqual(create.inner_text(), '')
            create_box = create.bounding_box()
            self.assertGreaterEqual(create_box['x'], 0)
            self.assertLessEqual(create_box['x'] + create_box['width'], width)
            self.assertGreaterEqual(create_box['height'], 44)
            if width <= 768:
                toggle_box = self.page.get_by_role('button', name='展开侧边栏', exact=True).bounding_box()
                self.assertGreaterEqual(create_box['x'], toggle_box['x'] + toggle_box['width'])
            else:
                self.page.get_by_role('button', name='收起侧栏', exact=True).click()
                expect(self.page.get_by_role('complementary', name='工作区导航').get_by_role('button', name='新对话', exact=True)).to_have_count(0)
                expect(create).to_be_visible()
            self.page.evaluate('document.fonts.ready')
            self.page.screenshot(path=str(captures / f'new-{width}.png'))
            self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'))
            boxes = [self.page.get_by_role('button', name=name, exact=True).bounding_box()
                     for name in ['选择 Agent', '选择工作目录', '配置 Agent']]
            self.assertLess(boxes[0]['x'], boxes[1]['x'])
            self.assertLess(boxes[1]['x'], boxes[2]['x'])
            for box in boxes:
                self.assertGreaterEqual(box['x'], 0)
                self.assertLessEqual(box['x'] + box['width'], width)
                self.assertGreaterEqual(box['height'], 44)
            menu = self.menu()
            box = menu.bounding_box()
            self.assertLessEqual(box['y'] + box['height'], height)
            self.assertGreaterEqual(box['x'], 0)
            self.assertLessEqual(box['x'] + box['width'], width)
            self.page.keyboard.press('Escape')

    def test_sidebar_only_renames_and_deletes_conversations(self):
        self.open_page()
        self.page.get_by_role('button', name='展开侧边栏', exact=True).click()
        expect(self.page.get_by_role('button', name='查看归档对话', exact=True)).to_have_attribute('aria-expanded', 'false')
        row = self.page.locator('.conversation-item').filter(has_text='移动端排版优化')
        row.get_by_role('button', name='更多操作', exact=True).click()
        menu = row.get_by_role('menu')
        expect(menu.get_by_role('menuitem')).to_have_text(['置顶对话', '重命名', '归档', '删除'])
        menu.get_by_role('menuitem', name='重命名', exact=True).click()
        title = self.page.locator('.conversation-title-input')
        title.fill('修改后的对话')
        title.press('Enter')
        row = self.page.locator('.conversation-item').filter(has_text='修改后的对话')
        expect(row.locator('.conversation-title')).to_have_text('修改后的对话')
        self.assertEqual(self.renames, [{'title': '修改后的对话'}])
        row = self.page.locator('.conversation-item').filter(has_text='修改后的对话')
        row.get_by_role('button', name='更多操作', exact=True).click()
        row.get_by_role('menuitem', name='删除', exact=True).click()
        confirmation = row.get_by_role('dialog')
        expect(confirmation).to_contain_text('确定永久删除这个对话吗？')
        confirmation.get_by_role('button', name='取消', exact=True).click()
        expect(confirmation).to_have_count(0)
        self.assertEqual(self.deletions, [])
        row.get_by_role('button', name='更多操作', exact=True).click()
        row.get_by_role('menuitem', name='删除', exact=True).click()
        row.get_by_role('dialog').get_by_role('button', name='永久删除', exact=True).click()
        expect(row).to_have_count(0)
        self.assertEqual(self.deletions, [2])
        expect(self.page.get_by_role('textbox', name='消息', exact=True)).to_be_enabled()

    def test_sidebar_pin_persists_and_does_not_select_the_conversation(self):
        self.open_page()
        self.page.get_by_role('button', name='展开侧边栏', exact=True).click()
        rows = self.page.locator('.conversation-item')
        row = rows.filter(has_text='新对话页面设计')
        row.get_by_role('button', name='更多操作', exact=True).click()
        row.get_by_role('menuitem', name='置顶对话', exact=True).click()
        expect(rows.first.locator('.conversation-title')).to_have_text('新对话页面设计')
        expect(row).to_have_class(re.compile(r'\bpinned\b'))
        self.assertEqual(self.pins, [{'is_pinned': True}])
        expect(self.page.locator('.new-conversation-title')).to_have_text('新对话')
        self.assertNotIn('conversation=', self.page.url)
        self.page.reload()
        self.page.get_by_role('button', name='展开侧边栏', exact=True).click()
        expect(rows.first.locator('.conversation-title')).to_have_text('新对话页面设计')
        row.get_by_role('button', name='更多操作', exact=True).click()
        row.get_by_role('menuitem', name='取消置顶', exact=True).click()
        expect(rows.first.locator('.conversation-title')).to_have_text('查询示例资料')
        expect(row.get_by_role('button', name='置顶对话', exact=True)).to_be_hidden()
        self.assertEqual(self.pins, [{'is_pinned': True}, {'is_pinned': False}])
        self.page.set_viewport_size(dict(width=1440, height=900))
        row.hover()
        more = row.get_by_role('button', name='更多操作', exact=True)
        more.focus()
        more.press('Enter')
        pin = row.get_by_role('menuitem', name='置顶对话', exact=True)
        expect(pin).to_be_focused()
        pin.press('Enter')
        expect(rows.first.locator('.conversation-title')).to_have_text('新对话页面设计')

    def test_sidebar_archives_can_expand_collapse_and_reopen(self):
        archived = dict(id=7, title='已归档的项目讨论', agent='firefly', message_count=2, is_archived=True)
        self.conversations.append(archived)
        self.open_page()
        self.page.get_by_role('button', name='展开侧边栏', exact=True).click()
        sidebar = self.page.get_by_role('complementary', name='工作区导航')
        archive_group = sidebar.get_by_role('group', name='归档对话', exact=True)
        expect(sidebar.get_by_role('button', name='查看更多对话', exact=True)).to_have_count(0)
        captures = Path('output/sidebar-archives')
        captures.mkdir(parents=True, exist_ok=True)
        for width, height in [(390, 844), (320, 568), (1440, 900)]:
            self.page.set_viewport_size(dict(width=width, height=height))
            toggle = sidebar.get_by_role('button', name='查看归档对话', exact=True)
            expect(toggle).to_have_attribute('aria-expanded', 'false')
            expect(archive_group).to_be_hidden()
            toggle.click()
            toggle = sidebar.get_by_role('button', name='收起归档对话', exact=True)
            expect(toggle).to_have_attribute('aria-expanded', 'true')
            expect(archive_group).to_contain_text(archived['title'])
            expect(archive_group).to_be_visible()
            expect(sidebar.locator('.conversation-item')).to_have_count(4)
            expect(sidebar.get_by_role('button', name='查看更多对话', exact=True)).to_have_count(0)
            self.assertLess(toggle.bounding_box()['y'], archive_group.bounding_box()['y'])
            self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'))
            self.page.screenshot(path=str(captures / f'expanded-{width}.png'))
            toggle.click()
            expect(archive_group).to_be_hidden()
            expect(sidebar.locator('.conversation-item')).to_have_count(3)
            expect(sidebar.get_by_role('button', name='查看归档对话', exact=True)).to_have_attribute('aria-expanded', 'false')
        self.assertEqual(self.archive_requests, 1)
        self.assertNotIn('conversation=', self.page.url)

    def test_mobile_sidebar_inline_search_single_rows_and_backdrop_close(self):
        titles = ['Codex开启自动审查', 'Firefly 项目迭代', '移动端侧边栏优化',
                  '查询示例资料', '新对话页面设计', 'GitHub Actions 卡片优化',
                  '修复实时消息同步', '独立工作目录整理',
                  '对话自动清理规则', '调整模型配置']
        self.conversations = [dict(id=index + 1, title=title, agent='codex', cwd='/workspace/firefly',
                                   message_count=2, is_pinned=index < 2)
                              for index, title in enumerate(titles)]
        self.conversations[3]['cwd'] = '/workspace/笔记'
        self.conversations[6].update(cwd='.assistant-parallel/copy/work',
                                     source_cwd='tools/codex-tools', workspace_mode='parallel')
        self.conversations[7]['cwd'] = 'tools/codex-tools'
        self.conversations[-1].update(agent='firefly', cwd=None)
        self.open_page()
        toggle = self.page.get_by_role('button', name='展开侧边栏', exact=True)
        toggle.click()
        sidebar = self.page.get_by_role('complementary', name='工作区导航')
        rows = sidebar.locator('.conversation-item')
        expect(rows).to_have_count(len(titles))
        rows.filter(has_text='移动端侧边栏优化').click()
        toggle.click()
        expect(sidebar).to_have_class(re.compile(r'\bopen\b'))
        expect(sidebar.get_by_role('button', name=re.compile(r'关闭|收起'))).to_have_count(0)
        expect(sidebar.locator('.conversation-meta')).to_have_count(0)
        self.page.evaluate('document.fonts.ready')
        expect(sidebar.locator('.conversation-directory-name')).to_have_text(
            ['firefly', 'firefly', 'firefly', '笔记', 'firefly', 'firefly', 'codex-tools', 'codex-tools', 'firefly', 'workspace'])
        self.assertTrue(rows.last.locator('.conversation-directory-name').evaluate(
            'element => element.scrollWidth <= element.clientWidth'))
        expect(sidebar.locator('.conversation-directory-prefix svg')).to_have_count(0)
        github_title = rows.filter(has_text='GitHub Actions 卡片优化').locator('.conversation-title-text')
        self.assertTrue(github_title.evaluate('element => element.scrollWidth <= element.clientWidth'))
        expect(rows.filter(has_text='修复实时消息同步').locator('.conversation-directory-prefix')).to_have_attribute(
            'aria-label', '创建目录：/tools/codex-tools')
        expect(sidebar.get_by_role('separator')).to_have_count(1)
        self.page.wait_for_function("Math.abs(document.querySelector('.tool-panel').getBoundingClientRect().x) < 0.5")
        self.page.evaluate('document.fonts.ready')
        captures = Path('output/mobile-sidebar-no-pin-marker')
        captures.mkdir(parents=True, exist_ok=True)
        self.page.mouse.move(370, 400)
        self.page.screenshot(path=str(captures / 'expanded.png'))
        for width, height in [(390, 844), (320, 568), (430, 932)]:
            self.page.set_viewport_size(dict(width=width, height=height))
            self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'))
            footer = sidebar.locator('.sidebar-settings-footer').bounding_box()
            self.assertLessEqual(footer['y'] + footer['height'], height)
            for row in rows.all():
                self.assertEqual(round(row.bounding_box()['height']), 44)
                self.assertEqual(row.locator('.conversation-title').evaluate(
                    'element => getComputedStyle(element).whiteSpace'), 'nowrap')
                directory = row.locator('.conversation-directory-prefix').bounding_box()
                self.assertLessEqual(directory['width'], 60)
                self.assertEqual(row.locator('.conversation-directory-prefix').evaluate(
                    'element => getComputedStyle(element).borderRightWidth'), '0px')
                title = row.locator('.conversation-title').bounding_box()
                title_text = row.locator('.conversation-title-text').bounding_box()
                self.assertAlmostEqual(title_text['x'], title['x'], delta=0.5)
                self.assertGreaterEqual(title['x'], directory['x'] + directory['width'])
                self.assertAlmostEqual(directory['y'] + directory['height'] / 2,
                                       title['y'] + title['height'] / 2, delta=1)
        self.page.set_viewport_size(dict(width=390, height=844))
        search = sidebar.get_by_role('searchbox', name='搜索对话', exact=True)
        search.fill('GitHub')
        expect(rows).to_have_count(1)
        expect(rows.first).to_contain_text('GitHub Actions 卡片优化')
        expect(self.page.get_by_role('dialog')).to_have_count(0)
        search.fill('tools/codex-tools')
        expect(rows).to_have_count(1)
        expect(rows.first.locator('.conversation-directory-name')).to_have_text('codex-tools')
        expect(rows.first.locator('.conversation-title-text')).to_have_text('修复实时消息同步')
        search.fill('不存在的标题')
        expect(sidebar.get_by_text('没有匹配的对话', exact=True)).to_be_visible()
        self.page.locator('.sidebar-backdrop').click(position=dict(x=374, y=150))
        expect(toggle).to_have_attribute('aria-expanded', 'false')
        expect(sidebar).to_have_attribute('inert', '')
        toggle.click()
        expect(search).to_have_value('')
        expect(rows).to_have_count(len(titles))
        expect(sidebar.get_by_role('button', name=re.compile('额度|余额'))).to_have_count(0)
        self.page.keyboard.press('Escape')
        expect(toggle).to_have_attribute('aria-expanded', 'false')

    def test_capture_mobile_and_desktop(self):
        self.open_page()
        self.choose_agent('Codex')
        captures = Path('output/new-chat-ui')
        captures.mkdir(parents=True, exist_ok=True)
        for name, width, height in [('mobile', 390, 844), ('desktop', 1440, 1024)]:
            self.page.set_viewport_size(dict(width=width, height=height))
            self.page.locator('.new-conversation-title').click()
            self.page.screenshot(path=str(captures / f'{name}.png'))
            self.menu()
            self.page.screenshot(path=str(captures / f'{name}-menu.png'))
            self.page.keyboard.press('Escape')
            self.page.get_by_role('button', name=re.compile(r'^(配置 Agent|收起 Agent 配置)$')).click()
            expect(self.page.get_by_role('group', name='Codex 配置', exact=True)).to_be_visible()
            self.page.screenshot(path=str(captures / f'{name}-config-inline.png'))
            self.page.get_by_role('button', name=re.compile(r'^(配置 Agent|收起 Agent 配置)$')).click()
            self.page.get_by_role('button', name='选择工作目录', exact=True).click()
            expect(self.page.get_by_role('group', name='选择工作目录', exact=True)).to_be_visible()
            self.page.screenshot(path=str(captures / f'{name}-workspace-inline.png'))
            self.page.get_by_role('button', name='选择工作目录', exact=True).click()


if __name__ == '__main__':
    unittest.main()
