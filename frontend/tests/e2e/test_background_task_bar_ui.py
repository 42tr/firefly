"""Background task bar regression with mocked task APIs. Start Vite on port 5193 first."""
import os
import re
import unittest
import urllib.request
from pathlib import Path
from playwright.sync_api import expect, sync_playwright


class BackgroundTaskBarUI(unittest.TestCase):
    def test_running_background_tasks_show_count_cancel_and_mobile(self):
        base = os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5193')
        source = urllib.request.urlopen(base + '/src/components/BackgroundTaskBar.vue').read().decode()
        vue_url = re.search(r'from "([^"]*vue\.js[^"]*)"', source).group(1)
        html = '''<!doctype html><html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
          <style>body{font-family:system-ui;margin:0;padding:16px;color:#24333c;background:#fafbfc;--muted:#63737e;--border-soft:#d6dde2;--focus-ring:#2563eb;}</style>
          <div id="app"></div><script type="module">
          import {createApp,h} from 'VUE_MODULE';
          import BackgroundTaskBar from '/src/components/BackgroundTaskBar.vue';
          createApp({render:()=>h(BackgroundTaskBar,{conversationId:1,jobId:'job',replyStatus:'running'})}).mount('#app');
          </script></html>'''.replace('VUE_MODULE', vue_url)

        tasks, cancelled = [], []

        def api(route):
            if route.request.method == 'POST':
                ids = route.request.post_data_json['task_ids']
                cancelled.append(ids)
                for task in tasks:
                    if task['id'] in ids:
                        task['status'] = 'cancelled'
                route.fulfill(json={'ok': True})
                return
            route.fulfill(json={'tasks': [dict(task) for task in tasks], 'cursor': 1, 'tool_cursor': 0, 'events': []})

        with sync_playwright() as p:
            browser = p.chromium.launch(headless=True)
            page = browser.new_page(viewport={'width': 1100, 'height': 850})
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.route('**/__background_task_test', lambda route: route.fulfill(content_type='text/html', body=html))
            page.route('**/api/assistant/conversations/1/tasks**', api)
            with page.expect_response(lambda response: response.request.method == 'GET' and '/tasks?' in response.url):
                page.goto(base + '/__background_task_test')
            expect(page.locator('.background-task-bar')).to_have_count(0)

            current = 1760000000000
            tasks.extend([
                dict(id='A', title='运行项目测试', generation_job_id='job', status='running', detached=True,
                     created_at=current, started_at=current),
                dict(id='B', title='检查实现结果', generation_job_id='job', status='queued', detached=True,
                     created_at=current),
                dict(id='C', title='读取文件', generation_job_id='job', status='running', detached=False,
                     created_at=current, started_at=current),
                dict(id='D', title='已完成任务', generation_job_id='job', status='succeeded', detached=True,
                     created_at=current, ended_at=current),
            ])
            expect(page.get_by_text('后台任务 2', exact=True)).to_be_visible(timeout=30000)
            self.assertEqual(page.locator('.background-task-bar ul, .background-task-bar li').count(), 0)

            page.set_viewport_size({'width': 390, 'height': 844})
            self.assertLessEqual(page.evaluate('document.documentElement.scrollWidth'), 390)
            Path('output').mkdir(exist_ok=True)
            page.screenshot(path='output/background-task-bar-mobile.png', full_page=True)

            with page.expect_request(lambda request: request.method == 'POST' and request.url.endswith('/tasks/cancel')) as posted:
                page.get_by_role('button', name='取消', exact=True).click()
            self.assertEqual(posted.value.post_data_json, {'task_ids': ['A', 'B']})
            expect(page.locator('.background-task-bar')).to_have_count(0, timeout=30000)
            self.assertEqual(cancelled, [['A', 'B']])
            self.assertEqual(errors, [])
            browser.close()


if __name__ == '__main__':
    unittest.main()
