"""Sidebar settings menu regression with mocked APIs and no live mutations.

Start Vite on 5199. FIREFLY_TEST_URL and FIREFLY_TEST_BROWSER override the target
and browser. Captures are saved under output/sidebar-settings-ui/.
"""
import json
import os
from pathlib import Path
from urllib.parse import urlparse
import unittest

from playwright.sync_api import expect
import test_new_conversation_ui as harness


class SidebarSettingsUI(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        harness.NewConversationUI.setUpClass.__func__(cls)

    def setUp(self):
        harness.NewConversationUI.setUp(self)
        self.authenticated = True
        self.logout_calls = 0
        self.agent_loads = 0
        self.agent_settings = {
            'revision': 0, 'profiles': [{
                'id': 'default', 'name': '默认模型', 'base_url': 'http://127.0.0.1/v1',
                'model': 'fixture-model', 'api_key': None, 'key_configured': False,
                'context_tokens': 32768,
            }],
            'firefly': {'id': 'firefly', 'name': 'Firefly', 'description': '',
                        'instruction': '原提示词', 'model_profile': 'default', 'tools': [],
                        'enabled': True, 'timeout_secs': 60},
            'subagents': [],
        }

    def tearDown(self):
        self.assertEqual(self.errors, [])

    def api(self, route):
        path = urlparse(route.request.url).path
        if path == '/api/assistant/ping':
            route.fulfill(status=200 if self.authenticated else 401,
                          content_type='application/json', body='{}')
        elif path == '/api/assistant/agent-settings':
            if route.request.method == 'PUT':
                self.agent_settings = route.request.post_data_json
                self.agent_settings['revision'] += 1
            else:
                self.agent_loads += 1
            route.fulfill(content_type='application/json', body=json.dumps(self.agent_settings))
        elif path == '/api/assistant/agent-settings/tools':
            route.fulfill(content_type='application/json', body='{"tools": []}')
        elif path == '/api/modules':
            route.fulfill(content_type='application/json', body=json.dumps({'modules': [{
                'id': 'sidebar-calendar', 'name': '示例日历模块', 'version': '1.0.0',
                'enabled': True, 'status': 'ready', 'settings': {}, 'enabled_jobs': [], 'jobs': [],
                'config_schema': {'properties': {}},
                'pages': [{'id': 'calendar', 'title': '示例日历', 'icon': 'calendar'}],
            }], 'diagnostics': []}))
        else:
            harness.NewConversationUI.api(self, route)

    def open_page(self, mobile=False, width=1280, height=900):
        context = self.browser.new_context(viewport={'width': width, 'height': height},
                                           is_mobile=mobile, has_touch=mobile)
        self.page = context.new_page()
        self.addCleanup(context.close)
        self.page.route('**/api/**', self.api)
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.page.add_init_script('''window.WebSocket = class {
            constructor() { this.readyState = 0; setTimeout(() => { this.readyState = 1; this.onopen?.() }, 0) }
            send() {} close() { this.readyState = 3 }
        };''')
        self.page.goto(os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5199/'))
        expect(self.page.get_by_role('heading', name='想聊点什么？')).to_be_visible(timeout=15000)
        if mobile:
            self.page.get_by_role('button', name='展开侧边栏', exact=True).tap()
        self.trigger = self.page.get_by_role('button', name='设置', exact=True)
        self.menu = self.page.get_by_role('menu', name='设置', exact=True)
        expect(self.trigger).to_be_visible()
        expect(self.page.get_by_role('menuitem', name='Agent 配置', exact=True)).to_have_count(0)
        expect(self.page.get_by_role('menuitem', name='扩展模块', exact=True)).to_have_count(0)
        expect(self.page.get_by_role('menuitem', name='退出登录', exact=True)).to_have_count(0)
        return self.trigger

    def open_menu(self, touch=False):
        self.trigger.tap() if touch else self.trigger.click()
        expect(self.menu).to_be_visible()
        expect(self.menu.get_by_role('menuitem')).to_have_text(['Agent 配置', '扩展模块', '退出登录'])
        expect(self.menu.get_by_role('menuitem').first).to_be_focused()
        trigger = self.trigger.bounding_box()
        popup = self.menu.bounding_box()
        self.assertLessEqual(popup['y'] + popup['height'], trigger['y'])
        self.assertGreaterEqual(popup['x'], 0)
        self.assertLessEqual(popup['x'] + popup['width'], self.page.viewport_size['width'])
        self.assertGreaterEqual(popup['y'], 0)
        return self.menu

    def capture(self, name):
        output = Path('output/sidebar-settings-ui')
        output.mkdir(parents=True, exist_ok=True)
        self.page.evaluate('document.fonts.ready')
        self.page.mouse.move(self.page.viewport_size['width'] - 10, 100)
        self.page.screenshot(path=str(output / (name + '.png')))

    def test_desktop_hidden_actions_keyboard_and_outside_close(self):
        self.open_page()
        self.capture('desktop-closed')
        self.open_menu()
        self.capture('desktop-open')
        self.page.keyboard.press('End')
        expect(self.menu.get_by_role('menuitem', name='退出登录', exact=True)).to_be_focused()
        self.page.keyboard.press('ArrowDown')
        expect(self.menu.get_by_role('menuitem', name='Agent 配置', exact=True)).to_be_focused()
        self.page.keyboard.press('Escape')
        expect(self.menu).to_have_count(0)
        expect(self.trigger).to_be_focused()
        self.trigger.press('ArrowUp')
        expect(self.menu.get_by_role('menuitem', name='退出登录', exact=True)).to_be_focused()
        self.page.keyboard.press('Tab')
        expect(self.menu).to_have_count(0)
        self.open_menu()
        self.page.get_by_role('heading', name='想聊点什么？').click()
        expect(self.menu).to_have_count(0)
        self.open_menu()
        self.trigger.click()
        expect(self.menu).to_have_count(0)

    def test_collapsed_popup_not_clipped_and_manager_restores_focus(self):
        self.open_page()
        self.page.get_by_role('button', name='收起侧栏', exact=True).click()
        expect(self.page.locator('.tool-panel')).to_have_class('tool-panel collapsed')
        self.capture('collapsed-closed')
        self.open_menu()
        self.capture('collapsed-open')
        sidebar = self.page.locator('.tool-panel').bounding_box()
        popup = self.menu.bounding_box()
        self.assertGreater(popup['width'], sidebar['width'])
        self.menu.get_by_role('menuitem', name='扩展模块', exact=True).click()
        dialog = self.page.get_by_role('dialog', name='扩展模块', exact=True)
        expect(dialog).to_be_visible()
        expect(dialog.get_by_role('button', name='示例日历模块')).to_be_visible()
        expect(self.menu).to_have_count(0)
        dialog.get_by_role('button', name='关闭', exact=True).click()
        expect(dialog).not_to_be_visible()
        expect(self.trigger).to_be_focused()
        self.assertEqual(self.page.locator('.modules-dialog').count(), 1)

    def test_mobile_touch_focus_transitions_and_narrow_viewport(self):
        self.open_page(mobile=True, width=390, height=844)
        self.capture('mobile-closed')
        self.open_menu(touch=True)
        self.capture('mobile-open')
        item = self.menu.get_by_role('menuitem', name='扩展模块', exact=True)
        item.evaluate('el => el.addEventListener("mousedown", () => document.activeElement.blur(), { once: true })')
        item.tap()
        dialog = self.page.get_by_role('dialog', name='扩展模块', exact=True)
        expect(dialog).to_be_visible()
        dialog.get_by_role('button', name='关闭', exact=True).tap()
        expect(self.trigger).to_be_focused()
        self.open_menu(touch=True)
        self.menu.get_by_role('menuitem', name='扩展模块', exact=True).tap()
        expect(dialog).to_be_visible()
        self.page.keyboard.press('Escape')
        expect(dialog).not_to_be_visible()
        expect(self.trigger).to_be_focused()
        expect(self.page.locator('.tool-panel')).to_have_class('tool-panel open')
        self.open_menu(touch=True)
        self.page.keyboard.press('Escape')
        expect(self.page.locator('.tool-panel')).to_have_class('tool-panel open')
        expect(self.menu).to_have_count(0)
        self.page.set_viewport_size({'width': 320, 'height': 568})
        self.open_menu(touch=True)
        for button in [self.trigger, *self.menu.get_by_role('menuitem').all()]:
            self.assertGreaterEqual(button.bounding_box()['height'], 44)
        self.capture('narrow-mobile-open')
        self.page.locator('.sidebar-backdrop').tap(position={'x': 300, 'y': 100})
        expect(self.menu).to_have_count(0)
        self.page.get_by_role('button', name='展开侧边栏', exact=True).tap()
        expect(self.menu).to_have_count(0)

    def test_module_pages_stay_in_function_menu_without_manager_entry(self):
        self.open_page()
        self.page.get_by_role('button', name='更多功能', exact=True).click()
        menu = self.page.get_by_role('menu', name='更多功能', exact=True)
        expect(menu.get_by_role('menuitem', name='示例日历', exact=True)).to_be_visible()
        expect(menu.get_by_role('menuitem', name='扩展模块', exact=True)).to_have_count(0)
        expect(menu.get_by_role('menuitem', name='Agent 配置', exact=True)).to_have_count(0)
        expect(menu.get_by_role('menuitem', name='退出登录', exact=True)).to_have_count(0)

    def test_agent_settings_load_on_demand_save_and_restore_focus_in_all_layouts(self):
        for layout in ['desktop', 'collapsed', 'mobile']:
            with self.subTest(layout=layout):
                mobile = layout == 'mobile'
                before = self.agent_loads
                self.open_page(mobile=mobile, width=390 if mobile else 1280,
                               height=844 if mobile else 900)
                if layout == 'collapsed':
                    self.page.get_by_role('button', name='收起侧栏', exact=True).click()
                self.open_menu(touch=mobile)
                self.assertEqual(self.agent_loads, before, 'opening settings must not load Agent configuration')
                item = self.menu.get_by_role('menuitem', name='Agent 配置', exact=True)
                if mobile:
                    item.evaluate('el => el.addEventListener("mousedown", () => document.activeElement.blur(), { once: true })')
                    item.tap()
                else:
                    self.page.keyboard.press('Enter')
                dialog = self.page.get_by_role('dialog', name='Agent 配置', exact=True)
                expect(dialog).to_be_visible()
                expect(self.menu).to_have_count(0)
                prompt = dialog.get_by_label('系统提示词', exact=True)
                expect(prompt).to_be_visible()
                prompt.fill(layout + ' 保存的提示词')
                dialog.get_by_role('button', name='保存配置', exact=True).click()
                expect(dialog.get_by_role('status')).to_have_text('已保存，从下一轮对话生效。')
                self.assertEqual(self.agent_settings['firefly']['instruction'], layout + ' 保存的提示词')
                self.capture('agent-settings-' + layout)
                dialog.get_by_role('button', name='关闭', exact=True).click()
                expect(dialog).not_to_be_visible()
                expect(self.trigger).to_be_focused()
                self.assertEqual(self.page.locator('.agent-settings-dialog').count(), 1)
                self.open_menu(touch=mobile)
                self.menu.get_by_role('menuitem', name='Agent 配置', exact=True).click()
                expect(prompt).to_have_value(layout + ' 保存的提示词')
                self.page.keyboard.press('Escape')
                expect(dialog).not_to_be_visible()
                expect(self.trigger).to_be_focused()

    def test_agent_settings_keeps_unsaved_draft_when_switching_to_mobile(self):
        self.open_page()
        self.open_menu()
        self.page.keyboard.press('Enter')
        dialog = self.page.get_by_role('dialog', name='Agent 配置', exact=True)
        prompt = dialog.get_by_label('系统提示词', exact=True)
        expect(prompt).to_be_visible()
        prompt.fill('切换布局时保留未保存草稿')
        self.page.set_viewport_size({'width': 390, 'height': 844})
        expect(dialog).to_be_visible()
        expect(prompt).to_have_value('切换布局时保留未保存草稿')
        dialog.get_by_role('button', name='保存配置', exact=True).click()
        expect(dialog.get_by_role('status')).to_have_text('已保存，从下一轮对话生效。')
        dialog.get_by_role('button', name='关闭', exact=True).click()
        expect(dialog).not_to_be_visible()
        expect(self.page.get_by_role('button', name='展开侧边栏', exact=True)).to_be_focused()

    def test_logout_uses_existing_auth_flow(self):
        self.open_page(mobile=True, width=390, height=844)

        def logout(route):
            self.assertEqual(route.request.method, 'POST')
            self.logout_calls += 1
            self.authenticated = False
            route.fulfill(status=204)

        self.page.route('**/auth/logout', logout)
        self.open_menu(touch=True)
        self.menu.get_by_role('menuitem', name='退出登录', exact=True).tap()
        expect(self.page.get_by_label('访问密码', exact=True)).to_be_visible(timeout=15000)
        self.assertEqual(self.logout_calls, 1)


if __name__ == '__main__':
    unittest.main()
