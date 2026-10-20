"""Touch and keyboard regression for dynamically installed module menu entries.

Build: npm --prefix frontend run build && cargo build --locked
Run: python3 frontend/tests/e2e/test_firefly_more_menu_ui.py
Uses an isolated host and the self-contained calendar protocol example.
"""
import unittest
from playwright.sync_api import expect
import test_modules_ui as harness


class FireflyMoreMenuUI(harness.ModuleHost):
    def setUp(self):
        self.context=self.browser.new_context(viewport={'width':390,'height':844},is_mobile=True,has_touch=True)
        self.addCleanup(self.context.close)
        headers={'Origin':self.base}
        response=self.context.request.post(self.base+'/auth',headers=headers,data={'password':'module-test-password'})
        self.assertTrue(response.ok,response.text())
        response=self.context.request.put(self.base+'/api/modules/personal-calendar',headers=headers,data={'enabled':True})
        self.assertTrue(response.ok,response.text())
        response=self.context.request.post(self.base+'/api/modules/personal-calendar/call',headers=headers,data={'name':'save_date','arguments':{'id':'menu-example','name':'示例纪念日','date':'10-13'}})
        self.assertTrue(response.ok,response.text())
        self.assertFalse(response.json().get('isError'),response.text())
        self.page=self.context.new_page();self.errors=[];self.open_requests=0
        self.page.on('pageerror',lambda error:self.errors.append(str(error)))
        self.page.on('request',self.track_open)
        self.page.goto(self.base)
        self.trigger=self.page.get_by_role('button',name='更多功能',exact=True)
        self.menu=self.page.get_by_role('menu',name='更多功能',exact=True)
        self.dialog=self.page.get_by_role('dialog',name='我的重要日期',exact=True)
        expect(self.trigger).to_be_visible(timeout=15000)

    def tearDown(self):
        self.assertEqual(self.errors,[])

    def track_open(self,request):
        if request.method=='POST' and request.url.endswith('/api/modules/personal-calendar/pages/calendar/open'):
            self.open_requests+=1

    def open_menu(self):
        self.tap(self.trigger)
        expect(self.menu).to_be_visible()
        expect(self.menu.get_by_role('menuitem').first).to_be_focused()
        return self.menu.get_by_role('menuitem',name='我的重要日期',exact=True)

    def tap(self,locator):
        expect(locator).to_be_visible()
        box=locator.bounding_box();self.assertIsNotNone(box)
        x,y=box['x']+box['width']/2,box['y']+box['height']/2
        self.assertTrue(0<=x<390 and 0<=y<844)
        self.page.touchscreen.tap(x,y)

    def assert_module_open(self):
        expect(self.dialog).to_be_visible()
        frame=self.dialog.frame_locator('iframe')
        expect(frame.get_by_label('名称',exact=True)).to_be_visible(timeout=20000)
        expect(frame.locator('li')).to_contain_text('示例纪念日')
        self.assertEqual(self.open_requests,1)
        expect(self.menu).not_to_be_visible()
        self.tap(self.dialog.get_by_role('button',name='关闭模块页面',exact=True))
        expect(self.dialog).not_to_be_visible();expect(self.trigger).to_be_focused()

    def test_touch_opens_module_and_restores_focus_after_closing(self):
        self.tap(self.open_menu());self.assert_module_open()

    def test_null_focusout_before_touch_click_does_not_swallow_selection(self):
        item=self.open_menu()
        item.evaluate('''element => {
            window.sawNullMenuFocusout = false;
            element.closest('[role="menu"]').addEventListener('focusout', event => {
                if (event.relatedTarget === null) window.sawNullMenuFocusout = true;
            });
            element.addEventListener('mousedown', () => document.activeElement.blur(), { once: true });
        }''')
        self.tap(item)
        self.page.wait_for_function('window.sawNullMenuFocusout',polling=20,timeout=5000)
        self.assert_module_open()

    def test_outside_touch_keyboard_and_external_focus_close_menu(self):
        self.open_menu();self.tap(self.page.locator('.new-conversation-title'))
        expect(self.menu).not_to_be_visible()
        self.open_menu();self.page.keyboard.press('Escape')
        expect(self.menu).not_to_be_visible();expect(self.trigger).to_be_focused()
        self.open_menu();self.page.keyboard.press('Tab');expect(self.menu).not_to_be_visible()
        self.open_menu();self.page.locator('.conversation-column textarea').focus()
        expect(self.menu).not_to_be_visible()

    def test_keyboard_selection_opens_same_module(self):
        self.trigger.focus();self.page.keyboard.press('ArrowDown');self.page.keyboard.press('Home')
        item=self.menu.get_by_role('menuitem',name='我的重要日期',exact=True)
        index=item.evaluate('el => Array.from(el.closest("[role=menu]").querySelectorAll("[role=menuitem]")).indexOf(el)')
        for _ in range(index):self.page.keyboard.press('ArrowDown')
        expect(item).to_be_focused();self.page.keyboard.press('Enter');self.assert_module_open()


if __name__=='__main__':
    unittest.main()
