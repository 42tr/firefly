"""Child-agent workflow regression. Start Vite on port 5193 first."""
import os
import re
import time
import unittest
import urllib.request
from pathlib import Path
from urllib.parse import parse_qs, urlparse
from playwright.sync_api import expect, sync_playwright


class SubagentWorkflowUI(unittest.TestCase):
    def test_live_steps_details_reload_and_mobile(self):
        base = os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5193')
        source = urllib.request.urlopen(base+'/src/components/WorkflowTree.vue').read().decode()
        vue_url = re.search(r'from "([^\"]*vue\.js[^\"]*)"', source).group(1)
        html = '''<!doctype html><html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
          <style>body{font-family:system-ui;padding:24px;color:#24333c;background:#fafbfc;--muted:#63737e;}</style>
          <div id="app"></div><script type="module">
          import {createApp,h,ref} from 'VUE_MODULE';
          import WorkflowTree from '/src/components/WorkflowTree.vue';
          const status = ref('running');
          window.setWorkflowStatus = value => { status.value = value; };
          createApp({render:()=>h(WorkflowTree,{conversationId:1,jobId:'job',replyStatus:status.value})}).mount('#app');
          </script></html>'''.replace('VUE_MODULE', vue_url)
        with sync_playwright() as p:
            browser = p.chromium.launch(headless=True)
            page = browser.new_page(viewport={'width':1100, 'height':850})
            errors, requests, tasks = [], [], []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.route('**/__subagent_workflow_test', lambda route: route.fulfill(content_type='text/html', body=html))
            page.route('**/api/assistant/conversations/1/tasks?**', lambda route: route.fulfill(json={
                'tasks':tasks, 'cursor':0, 'tool_cursor':len(tasks), 'events':[]}))

            def detail(route):
                query = parse_qs(urlparse(route.request.url).query, keep_blank_values=True)
                requests.append(query)
                output = '本周周报' if query['call_id'] == ['delegate'] else False
                route.fulfill(json={'input':{'task':'生成本周周报'}, 'output':output, 'status':'completed'})

            page.route('**/api/assistant/conversations/1/tool-detail?**', detail)
            page.goto(base+'/__subagent_workflow_test')
            expect(page.get_by_text('正在处理', exact=True)).to_be_visible()
            current = int(time.time()*1000)
            common = dict(generation_job_id='job', kind='tool', created_at=current-12000,
                          started_at=current-12000, status='succeeded', ended_at=current-1000)
            tasks.extend([
                dict(common, id='special', call_id='special', title='查询重要日期'),
                dict(common, id='weekly', call_id='delegate', kind='handoff', title='weekly agent', summary='生成本周周报', status='running', ended_at=None),
                dict(common, id='history', call_id='history', parent_call_id='delegate', parent_task_id='weekly', title='查询本周工作记录'),
                dict(common, id='todos', call_id='todos', parent_call_id='delegate', parent_task_id='weekly', title='查询待办记录', status='running', ended_at=None),
                dict(common, id='timeout', call_id='timeout', title='搜索项目配置', status='timed_out',
                     created_at=current-183000, started_at=current-183000, ended_at=current),
                dict(common, id='unknown', call_id='unknown', title='检查旧任务', status='unknown', ended_at=None),
                dict(common, id='deep', call_id='deep', parent_call_id='delegate', parent_task_id='todos', title='检查待办详情'),
            ])
            expect(page.get_by_text('weekly agent', exact=True)).to_be_visible()
            expect(page.locator('[data-task-id="weekly"] > ul [data-task-id="history"]')).to_be_visible()
            expect(page.locator('[data-task-id="todos"] > .workflow-row .workflow-state')).to_have_text('执行中')
            expect(page.locator('[data-task-id="weekly"] > ul [data-task-id="todos"] > ul [data-task-id="deep"]')).to_be_visible()
            expect(page.locator('[data-task-id="timeout"] > .workflow-row .workflow-time')).to_have_text('执行 3 分 3 秒')
            expect(page.get_by_text('执行超时', exact=True)).to_be_visible()
            expect(page.locator('[data-task-id="weekly"] > .workflow-row .workflow-agent-summary')).to_have_text('生成本周周报')
            expect(page.locator('[data-task-id="unknown"] > .workflow-row .workflow-time')).to_have_text('耗时未记录')
            self.assertEqual(requests, [])
            expect(page.locator('[data-task-id="weekly"] > .workflow-row .workflow-cancel')).to_have_count(0)
            page.get_by_role('button', name='查询本周工作记录', exact=False).click()
            expect(page.get_by_text('执行结果', exact=True)).to_be_visible()
            self.assertEqual(requests[0]['call_id'], ['history'])
            self.assertEqual(requests[0]['parent_id'], ['delegate'])
            self.assertEqual(requests[0]['job_id'], ['job'])
            page.get_by_text('执行结果', exact=True).click()
            expect(page.locator('.workflow-output').last).to_have_text('false')
            page.get_by_role('button', name='查询本周工作记录', exact=False).click()
            page.get_by_role('button', name='检查待办详情', exact=False).click()
            expect(page.locator('[data-task-id="deep"] > .workflow-detail')).to_be_visible()
            requests_before_collapse = len(requests)
            page.get_by_role('button', name='weekly agent', exact=False).click()
            expect(page.locator('[data-task-id="weekly"] > .workflow-row .workflow-toggle')).to_have_attribute('aria-expanded', 'false')
            expect(page.locator('[data-task-id="weekly"] > ul')).to_have_count(0)
            expect(page.locator('[data-task-id="weekly"] .workflow-detail')).to_have_count(0)
            expect(page.get_by_text('查询重要日期', exact=True)).to_be_visible()
            expect(page.locator('[data-task-id="weekly"] > .workflow-row .workflow-agent-summary')).to_be_visible()
            self.assertEqual(len(requests), requests_before_collapse, 'collapsing an agent does not load its details')
            page.get_by_role('button', name='weekly agent', exact=False).click()
            expect(page.get_by_text('子 agent 返回结果', exact=True)).to_be_visible()
            expect(page.locator('[data-task-id="weekly"] > ul [data-task-id="history"]')).to_be_visible()
            expect(page.get_by_text('命令输出', exact=False)).to_have_count(0)
            self.assertEqual(requests[-1]['call_id'], ['delegate'])
            self.assertEqual(requests[-1]['parent_id'], [''])
            page.get_by_role('button', name='weekly agent', exact=False).click()
            tasks[0].update(status='running', ended_at=None)
            tasks[1].update(status='succeeded', ended_at=current, summary='已生成本周周报并汇总待办进展')
            tasks[3].update(status='succeeded', ended_at=current)
            expect(page.locator('[data-task-id="weekly"] > .workflow-row .workflow-state')).to_have_text('已完成')
            expect(page.locator('[data-task-id="weekly"] > .workflow-row .workflow-agent-summary')).to_have_text('已生成本周周报并汇总待办进展')
            expect(page.locator('[data-task-id="weekly"] > ul')).to_have_count(0)
            timer = page.locator('[data-task-id="weekly"] > .workflow-row .workflow-time')
            expect(timer).to_have_text('执行 12 秒')
            page.wait_for_timeout(1500)
            expect(timer).to_have_text('执行 12 秒')
            expect(page.get_by_text('正在处理', exact=True)).to_be_visible()
            tasks[0].update(status='succeeded', ended_at=current)
            page.evaluate("window.setWorkflowStatus('completed')")
            page.get_by_role('button', name='weekly agent', exact=False).click()
            expect(page.locator('[data-task-id="todos"] > .workflow-row .workflow-state')).to_have_text('已完成')
            expect(page.get_by_text('任务有异常', exact=True)).to_be_visible()
            page.get_by_role('button', name='收起执行流程', exact=True).click()
            expect(page.locator('[data-task-id]')).to_have_count(0)
            page.get_by_role('button', name='展开执行流程', exact=True).click()
            expect(page.get_by_text('查询本周工作记录', exact=True)).to_be_visible()
            page.reload()
            expect(page.get_by_text('weekly agent', exact=True)).to_be_visible()
            expect(page.locator('[data-task-id="weekly"] > .workflow-row .workflow-agent-summary')).to_have_text('已生成本周周报并汇总待办进展')
            page.evaluate("window.setWorkflowStatus('completed')")
            expect(page.locator('[data-task-id="weekly"] > ul [data-task-id="todos"]')).to_be_visible()
            Path('output').mkdir(exist_ok=True)
            page.screenshot(path='output/subagent-workflow-desktop.png', full_page=True)
            page.set_viewport_size({'width':390, 'height':844})
            self.assertLessEqual(page.evaluate('document.documentElement.scrollWidth'), 390)
            page.screenshot(path='output/subagent-workflow-mobile.png', full_page=True)
            self.assertEqual(errors, [])
            browser.close()


if __name__ == '__main__':
    unittest.main()
