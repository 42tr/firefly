"""End-to-end module installation against an isolated Firefly instance.

Build first: npm --prefix frontend run build && cargo build --locked
Then: python3 frontend/tests/e2e/test_modules_ui.py
FIREFLY_TEST_BINARY can select a prebuilt binary. No model credentials required.
"""
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import unittest
import urllib.request

from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[3]


class ModuleHost(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix='firefly-modules-')
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.directory = Path(cls.temporary.name)
        shutil.copytree(ROOT / 'examples/modules/calendar', cls.directory / 'modules/calendar')
        shutil.copy2(ROOT / 'frontend/examples/calendar/calendar.html', cls.directory / 'modules/calendar/calendar.html')
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        cls.base = f'http://127.0.0.1:{port}'
        binary = Path(os.getenv('FIREFLY_TEST_BINARY', ROOT / 'target/debug/firefly')).resolve()
        environment = {
            'PATH': os.environ['PATH'], 'HOME': str(cls.directory),
            'AUTH_PASSWORD': 'module-test-password', 'JWT_SECRET': 'module-tests-signing-key-32-bytes-long',
            'BIND_ADDRESS': f'127.0.0.1:{port}', 'COOKIE_SECURE': 'false',
            'RUST_LOG': 'warn',
        }
        environment.update(getattr(cls, 'environment_overrides', {}))
        subprocess.run([str(binary), '--init-db', f'sqlite://{cls.directory}/firefly.sqlite3'], cwd=cls.directory, env=environment, check=True, capture_output=True)
        cls.log = open(cls.directory / 'server.log', 'w+')
        cls.addClassCleanup(cls.log.close)
        cls.server = subprocess.Popen([str(binary)], cwd=cls.directory, env=environment, stdout=cls.log, stderr=subprocess.STDOUT)
        cls.addClassCleanup(cls.stop_server)
        for _ in range(150):
            if cls.server.poll() is not None:
                cls.log.seek(0)
                raise RuntimeError(cls.log.read())
            try:
                urllib.request.urlopen(cls.base + '/health', timeout=1)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError('Test server did not become healthy')
        cls.playwright = sync_playwright().start()
        cls.addClassCleanup(cls.playwright.stop)
        cls.browser = cls.playwright.chromium.launch(headless=True)
        cls.addClassCleanup(cls.browser.close)

    @classmethod
    def stop_server(cls):
        cls.server.terminate()
        try:
            cls.server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            cls.server.kill()
            cls.server.wait()


class ModulesUI(ModuleHost):
    def test_install_configure_page_tools_and_disable(self):
        context = self.browser.new_context(viewport={'width': 1280, 'height': 900})
        self.addCleanup(context.close)
        response = context.request.post(self.base + '/auth', headers={'Origin': self.base}, data={'password': 'module-test-password'})
        self.assertTrue(response.ok)
        page = context.new_page()
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(self.base)
        menu = page.get_by_role('button', name='更多功能', exact=True)
        page.get_by_role('button', name='设置', exact=True).click()
        page.get_by_role('menuitem', name='扩展模块', exact=True).click()
        manager = page.get_by_role('dialog', name='扩展模块', exact=True)
        expect(manager).to_be_visible()
        manager.get_by_role('button', name='重要日期示例').click()
        manager.get_by_label('启用模块', exact=True).check()
        manager.get_by_role('button', name='保存配置', exact=True).click()
        expect(manager.get_by_role('status')).to_have_text('已保存')
        manager.get_by_role('button', name='检查工具与执行记录').click()
        expect(manager.get_by_role('heading', name='工具测试', exact=True)).to_be_visible()
        manager.get_by_role('button', name='关闭', exact=True).click()
        menu.click()
        page.get_by_role('menuitem', name='我的重要日期', exact=True).click()
        dialog = page.get_by_role('dialog', name='我的重要日期', exact=True)
        expect(dialog).to_be_visible()
        frame = dialog.frame_locator('iframe')
        expect(frame.get_by_role('status')).to_contain_text('还没有记录', timeout=20000)
        frame.get_by_label('名称', exact=True).fill('测试周年纪念')
        frame.get_by_label('日期（月-日）').fill('10-13')
        frame.get_by_role('button', name='添加日期', exact=True).click()
        expect(frame.locator('li')).to_contain_text('测试周年纪念')
        sandboxed = frame.locator('body').evaluate('''() => {
            try { void parent.document.body; return false } catch { return true }
        }''')
        self.assertTrue(sandboxed)
        page.set_viewport_size({'width': 390, 'height': 844})
        expect(dialog).to_be_visible()
        self.assertLessEqual(dialog.bounding_box()['width'], 390)
        screenshot = ROOT / 'output/modules-ui.png'
        screenshot.parent.mkdir(exist_ok=True)
        page.screenshot(path=str(screenshot))
        dialog.get_by_role('button', name='关闭模块页面').click()
        page.reload()
        menu.click()
        page.get_by_role('menuitem', name='我的重要日期', exact=True).click()
        frame = page.get_by_role('dialog', name='我的重要日期', exact=True).frame_locator('iframe')
        expect(frame.locator('li')).to_contain_text('测试周年纪念', timeout=15000)
        page.get_by_role('button', name='关闭模块页面').click()
        page.get_by_role('button', name='展开侧边栏', exact=True).click()
        page.get_by_role('button', name='设置', exact=True).click()
        page.get_by_role('menuitem', name='扩展模块', exact=True).click()
        manager.get_by_role('button', name='重要日期示例').click()
        manager.get_by_label('启用模块', exact=True).uncheck()
        manager.get_by_role('button', name='保存配置', exact=True).click()
        expect(manager.get_by_role('status')).to_have_text('已保存')
        self.assertFalse(context.request.post(self.base + '/api/modules/personal-calendar/call', headers={'Origin': self.base}, data={'name': 'list_dates', 'arguments': {}}).ok)
        anonymous = self.browser.new_context()
        self.addCleanup(anonymous.close)
        self.assertEqual(anonymous.request.get(self.base + '/api/modules/personal-calendar/pages/calendar/html').status, 401)
        self.assertEqual(errors, [])


if __name__ == '__main__':
    unittest.main()
