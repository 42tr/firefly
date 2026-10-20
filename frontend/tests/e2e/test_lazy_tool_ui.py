"""Lazy tool I/O regression. Start Vite on port 5193 first."""
import os
import re
import unittest
import urllib.request
from pathlib import Path
from urllib.parse import parse_qs, urlparse
from playwright.sync_api import expect, sync_playwright

class LazyToolUI(unittest.TestCase):
    def test_process_and_child_tools_fetch_only_when_clicked(self):
        base = os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5193')
        source = urllib.request.urlopen(base + '/src/components/ProcessGroup.vue').read().decode()
        vue_url = re.search(r'from "([^\"]*vue\.js[^\"]*)"', source).group(1)
        html = '''<!doctype html><html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
        <style>body{font-family:system-ui;margin:24px;--muted:#666;--border-soft:#ddd;--bg-subtle:#f7f7f7;--bg-panel:#fff;--text-secondary:#444;--success-text:#287450;--focus-ring:#2563eb;}</style>
        <div id="app"></div><script type="module">
        import {createApp,h} from 'VUE_MODULE';
        import ProcessGroup from '/src/components/ProcessGroup.vue';
        createApp({render:()=>h(ProcessGroup,{conversationId:1,jobId:'job',
          steps:[{id:'root',kind:'tool-run',name:'command',status:'completed'},
            {id:'handoff',kind:'handoff',agent:'programmer',status:'completed',tools:[{id:'child',name:'command',status:'completed'}]}],
          toolStatusText:()=> '已完成',formatToolValue:value=>JSON.stringify(value,null,2),renderMessageHtml:()=>''})}).mount('#app');
        </script></html>'''.replace('VUE_MODULE',vue_url)
        with sync_playwright() as p:
            browser = p.chromium.launch(headless=True)
            page = browser.new_page(viewport={'width':1100,'height':850})
            requests, errors = [], []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.route('**/__lazy_tool_test', lambda route: route.fulfill(content_type='text/html',body=html))
            def detail(route):
                query = parse_qs(urlparse(route.request.url).query,keep_blank_values=True)
                requests.append(query)
                call = query['call_id'][0]
                route.fulfill(json={'id':call,'input':{'command':f'private input {call}'},'output':f'private output {call}','status':'completed'})
            page.route('**/api/assistant/conversations/1/tool-detail?**',detail)
            page.goto(base+'/__lazy_tool_test')
            expect(page.get_by_role('button',name='展开执行过程')).to_be_visible()
            self.assertEqual(requests, [])
            page.get_by_role('button',name='展开执行过程').click()
            expect(page.locator('.command-group')).not_to_have_attribute('open','')
            expect(page.locator('.handoff-panel')).not_to_have_attribute('open','')
            self.assertEqual(requests, [])
            page.locator('.command-group > summary').click()
            expect(page.locator('.command-entry')).not_to_have_attribute('open','')
            self.assertEqual(requests, [])
            page.locator('.command-entry > summary').click()
            expect(page.get_by_text('private output root',exact=True)).to_be_visible()
            self.assertEqual([request['call_id'][0] for request in requests], ['root'])
            page.locator('.command-entry > summary').click()
            page.locator('.command-entry > summary').click()
            expect(page.get_by_text('private output root',exact=True)).to_be_visible()
            self.assertEqual(len(requests),1)
            page.locator('.handoff-panel > summary').click()
            expect(page.get_by_text('private output handoff',exact=True)).to_be_visible()
            expect(page.locator('.handoff-tools .lazy-tool')).not_to_have_attribute('open','')
            self.assertEqual(len(requests),2)
            page.locator('.handoff-tools .lazy-tool > summary').click()
            expect(page.get_by_text('private output child',exact=True)).to_be_visible()
            self.assertEqual(requests[-1]['parent_id'], ['handoff'])
            page.set_viewport_size({'width':390,'height':844})
            self.assertLessEqual(page.evaluate('document.documentElement.scrollWidth'),390)
            Path('output').mkdir(exist_ok=True)
            page.screenshot(path='output/lazy-tool-mobile.png',full_page=True)
            page.get_by_role('button',name='收起执行过程').click()
            expect(page.locator('.process-group-body')).to_have_count(0)
            self.assertEqual(errors, [])
            browser.close()

if __name__ == '__main__':
    unittest.main()
