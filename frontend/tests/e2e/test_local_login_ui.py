"""Local password login regression. Start Vite before running this script.

FIREFLY_TEST_URL defaults to http://127.0.0.1:5189/.
"""
import json
import os
import unittest
from urllib.parse import urlparse

from playwright.sync_api import expect, sync_playwright


class LocalLoginUI(unittest.TestCase):
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
        authenticated = False

        def api(route):
            path = urlparse(route.request.url).path
            if path == '/api/assistant/ping':
                route.fulfill(status=200 if authenticated else 401, content_type='application/json', body='{}')
                return
            data = {}
            if path.endswith('/conversations'):
                data = {'conversations': []}
            elif path.endswith('/firefly/messages'):
                data = {'turns': [], 'has_more': False}
            route.fulfill(content_type='application/json', body=json.dumps(data))

        def auth(route):
            nonlocal authenticated
            if route.request.method != 'POST':
                route.continue_()
                return
            if urlparse(route.request.url).path == '/auth/logout':
                authenticated = False
                route.fulfill(status=204)
                return
            payload = route.request.post_data_json
            self.assertEqual(set(payload), {'password'})
            authenticated = payload['password'] == 'private-password'
            route.fulfill(status=204 if authenticated else 401)

        page.route('**/api/**', api)
        page.route('**/auth/**', auth)
        page.goto(os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5189/'))
        password = page.get_by_label('访问密码', exact=True)
        expect(password).to_be_visible(timeout=15000)
        self.assertEqual(page.locator('input[name="username"]').count(), 0)
        self.assertEqual(page.locator('input[name="password"]').count(), 1)
        self.assertEqual(password.get_attribute('autocomplete'), 'current-password')
        box = page.locator('.login-card').bounding_box()
        self.assertTrue(box['x'] >= 0 and box['x'] + box['width'] <= width)
        expect(page.get_by_role('button', name='进入流萤', exact=True)).to_be_disabled()
        password.fill('wrong-password')
        password.press('Enter')
        expect(page.get_by_role('alert')).to_have_text('访问密码错误')
        expect(password).to_have_value('')
        password.fill('private-password')
        password.press('Enter')
        expect(password).not_to_be_visible()
        expect(page.get_by_role('heading', name='想聊点什么？', exact=True)).to_be_visible()
        if width < 768:
            page.get_by_role('button', name='展开侧边栏', exact=True).click()
            expect(page.locator('.tool-panel')).to_have_class('tool-panel open')
        page.get_by_role('button', name='设置', exact=True).click()
        page.get_by_role('menu', name='设置', exact=True).get_by_role('menuitem', name='退出登录', exact=True).click()
        expect(page.get_by_label('访问密码', exact=True)).to_be_visible(timeout=15000)
        self.assertFalse(authenticated)
        self.assertEqual(errors, [])

    def test_desktop_login_and_logout(self):
        self.exercise(1280, 800)

    def test_mobile_login_and_logout(self):
        self.exercise(390, 844)


if __name__ == '__main__':
    unittest.main()
