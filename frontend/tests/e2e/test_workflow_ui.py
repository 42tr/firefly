"""Workflow tree regression with mocked task APIs. Start Vite on port 5193 first."""
import json
import os
import time
import unittest
import urllib.request
import re
from pathlib import Path
from playwright.sync_api import expect, sync_playwright

class WorkflowUI(unittest.TestCase):
    def test_background_tree_logs_cancel_refresh_and_mobile(self):
        with sync_playwright() as p:
            browser = p.chromium.launch(headless=True)
            page = browser.new_page(viewport={"width": 1100, "height": 850})
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            current = int(time.time() * 1000)
            tasks = [dict(id='B', title='Codex：实现登录功能', generation_job_id='job', status='running', detached=True,
                          created_at=current-60000, started_at=current-60000, deadline_at=current+7200000,
                          arguments={'cwd':'/workspace/project','command':'codex exec "实现登录功能"'}),
                     dict(id='D', parent_task_id='B', title='检查实现结果', generation_job_id='job', status='queued',
                          detached=True, created_at=current-10000, arguments={}),
                     dict(id='C', title='检查接口文档', generation_job_id='job', status='succeeded',
                          created_at=current-50000, started_at=current-50000, ended_at=current-40000,
                          arguments={}, result={'checked':True})]
            cancelled = []
            detail_requests = []
            def api(route):
                if re.search(r'/tasks/[BCD]$', route.request.url):
                    task_id = route.request.url.rsplit('/',1)[1]
                    detail_requests.append(task_id)
                    task = next(task for task in tasks if task['id'] == task_id)
                    route.fulfill(json={**task, 'output':'正在执行测试\n<script>never execute</script>\n' if task_id == 'B' else ''})
                elif route.request.url.endswith('/cancel'):
                    ids = route.request.post_data_json['task_ids']
                    cancelled.extend(ids)
                    for task in tasks:
                        if task['id'] in ids or task.get('parent_task_id') in ids:
                            task['status'] = 'cancelled'
                            task['ended_at'] = current
                    route.fulfill(json={'ok':True})
                else:
                    route.fulfill(json={'tasks':[{k:v for k,v in task.items() if k not in ['arguments','result','error']} for task in tasks],'cursor':1,'events':[]})
            page.route('**/api/assistant/conversations/1/tasks**',api)
            base=os.getenv('FIREFLY_TEST_URL','http://127.0.0.1:5193')
            source=urllib.request.urlopen(base+'/src/components/WorkflowTree.vue').read().decode()
            vue_url=re.search(r'from "([^"]*vue\.js[^\"]*)"',source).group(1)
            html = '''<!doctype html><html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
              <style>body{font-family:system-ui;padding:24px;color:#24333c;background:#fafbfc;--muted:#63737e;}</style>
              <div id="app"></div><script type="module">
              import {createApp,h} from 'VUE_MODULE';
              import WorkflowTree from '/src/components/WorkflowTree.vue';
              createApp({render:()=>h(WorkflowTree,{conversationId:1,jobId:'job',replyStatus:'completed'})}).mount('#app');
              </script></html>'''.replace('VUE_MODULE',vue_url)
            page.route('**/__workflow_test',lambda route:route.fulfill(content_type='text/html',body=html))
            url=os.getenv('FIREFLY_TEST_URL','http://127.0.0.1:5193')+'/__workflow_test'
            page.goto(url)
            expect(page.get_by_text('后台任务 2',exact=True)).to_be_visible(timeout=30000)
            expect(page.get_by_text('回答已结束',exact=True)).to_be_visible()
            expect(page.get_by_text('检查实现结果',exact=True)).to_be_visible()
            expect(page.locator('.workflow-detail')).to_have_count(0)
            self.assertEqual(detail_requests, [])
            toggle=page.get_by_role('button',name='Codex：实现登录功能',exact=False)
            toggle.click()
            expect(page.get_by_text('检查实现结果',exact=True)).to_have_count(0)
            self.assertEqual(detail_requests, [])
            toggle.click()
            expect(page.locator('.workflow-command')).to_contain_text('codex exec')
            expect(page.locator('.workflow-detail details')).not_to_have_attribute('open','')
            page.get_by_text('命令输出',exact=False).click()
            expect(page.locator('.workflow-output')).to_contain_text('<script>never execute</script>')
            self.assertTrue(detail_requests)
            self.assertEqual(set(detail_requests), {'B'})
            toggle.click()
            expect(page.locator('.workflow-detail')).to_have_count(0)
            closed_count = len(detail_requests)
            page.wait_for_timeout(2200)
            self.assertEqual(len(detail_requests), closed_count)
            expect(page.get_by_text('检查实现结果',exact=True)).to_have_count(0)
            toggle.click()
            expect(page.locator('.workflow-command')).to_be_visible()
            page.set_viewport_size({'width':390,'height':844})
            self.assertLessEqual(page.evaluate('document.documentElement.scrollWidth'),390)
            Path('output').mkdir(exist_ok=True)
            page.screenshot(path='output/workflow-mobile.png',full_page=True)
            page.set_viewport_size({'width':1100,'height':850})
            page.screenshot(path='output/workflow-desktop.png',full_page=True)
            page.locator('[data-task-id="B"] > .workflow-row').get_by_role('button',name='取消此分支').click()
            expect(page.locator('[data-task-id="B"] > .workflow-row .workflow-state')).to_have_text('已取消')
            self.assertEqual(cancelled,['B'])
            page.reload()
            expect(page.get_by_text('任务已结束',exact=True)).to_be_visible(timeout=30000)
            self.assertEqual(errors,[])
            browser.close()

if __name__ == '__main__':
    unittest.main()
