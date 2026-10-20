"""Exercise the packaged Rust / UI SDK example against an isolated Firefly host.

Build first: npm --prefix frontend run build && cargo build --locked && npm --prefix frontend run build:example:module-sdk
Run: python3 frontend/tests/e2e/test_rust_sdk_module_ui.py
Uses temporary module data and the existing authenticated module-host harness.
"""
import shutil
import unittest

from playwright.sync_api import expect
import test_modules_ui as harness


class RustSdkModuleUI(harness.ModuleHost):
    @classmethod
    def setUpClass(cls):
        package = harness.ROOT / 'output/modules/rust-sdk-counter'
        for name in ['module.toml', 'config.schema.json', 'ui.html', 'firefly-sdk-counter-example']:
            if not (package / name).is_file():
                raise RuntimeError('Build the example first: npm --prefix frontend run build:example:module-sdk')
        super().setUpClass()
        shutil.copytree(package, cls.directory / 'modules/rust-sdk-counter')

    def test_sdk_page_tools_configuration_and_persistence(self):
        context = self.browser.new_context(viewport={'width': 1280, 'height': 900})
        self.addCleanup(context.close)
        headers = {'Origin': self.base}
        response = context.request.post(self.base + '/auth', headers=headers,
                                        data={'password': 'module-test-password'})
        self.assertTrue(response.ok, response.text())
        response = context.request.post(self.base + '/api/modules/reload', headers=headers)
        self.assertTrue(response.ok, response.text())

        def configure(enabled=True, step=4):
            response = context.request.put(self.base + '/api/modules/rust-sdk-counter', headers=headers,
                                           data={'enabled': enabled, 'settings': {'label': '示例计数', 'step': step}})
            self.assertTrue(response.ok, response.text())

        configure()
        page = context.new_page()
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(self.base)

        def open_counter():
            page.get_by_role('button', name='更多功能', exact=True).click()
            page.get_by_role('menuitem', name='SDK 计数器', exact=True).click()
            dialog = page.get_by_role('dialog', name='SDK 计数器', exact=True)
            expect(dialog).to_be_visible()
            frame = dialog.frame_locator('iframe')
            expect(frame.get_by_role('heading', name='示例计数', exact=True)).to_be_visible(timeout=20000)
            return frame

        frame = open_counter()
        value = frame.get_by_label('当前数值', exact=True)
        expect(value).to_have_text('0')
        frame.get_by_role('button', name='增加', exact=True).click()
        expect(value).to_have_text('4')
        frame.get_by_role('button', name='关闭', exact=True).click()
        expect(page.get_by_role('dialog', name='SDK 计数器', exact=True)).not_to_be_visible()
        frame = open_counter()
        expect(frame.get_by_label('当前数值', exact=True)).to_have_text('4')
        page.get_by_role('button', name='关闭模块页面', exact=True).click()
        expect(page.get_by_role('dialog', name='SDK 计数器', exact=True)).not_to_be_visible()

        # Saving config restarts the MCP process; the database stays in its data directory.
        configure(step=2)
        frame = open_counter()
        value = frame.get_by_label('当前数值', exact=True)
        expect(value).to_have_text('4')
        frame.get_by_role('button', name='增加', exact=True).click()
        expect(value).to_have_text('6')
        result = context.request.post(self.base + '/api/modules/rust-sdk-counter/call', headers=headers,
                                      data={'name': 'read', 'arguments': {'path': '/counter'}})
        self.assertTrue(result.ok, result.text())
        self.assertEqual(result.json()['structuredContent']['body']['value'], 6)
        for width, height in [(390, 844), (320, 568)]:
            page.set_viewport_size({'width': width, 'height': height})
            self.assertTrue(frame.locator('body').evaluate('document.documentElement.scrollWidth <= innerWidth'))
        capture = harness.ROOT / 'output/rust-sdk-module-ui.png'
        capture.parent.mkdir(exist_ok=True)
        page.screenshot(path=str(capture))
        frame.get_by_role('button', name='清零', exact=True).click()
        expect(value).to_have_text('0')
        frame.get_by_role('button', name='关闭', exact=True).click()
        configure(enabled=False, step=2)
        rejected = context.request.post(self.base + '/api/modules/rust-sdk-counter/call', headers=headers,
                                        data={'name': 'read', 'arguments': {'path': '/counter'}})
        self.assertFalse(rejected.ok)
        self.assertTrue((self.directory / 'storage/modules/rust-sdk-counter/counter.sqlite3').is_file())
        self.assertEqual(errors, [])


if __name__ == '__main__':
    unittest.main()
