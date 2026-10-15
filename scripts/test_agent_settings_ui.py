"""Agent configuration and dynamic handoffs against an isolated local instance.

Build: npm run build && cargo build --locked
Run: python3 scripts/test_agent_settings_ui.py
Uses a local fake Responses service; no model credentials or external network.
"""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import threading
import time

from playwright.sync_api import expect
from test_modules_ui import ModuleHost


class ResponsesHandler(BaseHTTPRequestHandler):
    requests = []

    def log_message(self, *_args):
        pass

    def do_GET(self):
        payload = json.dumps({'id': self.path.rsplit('/', 1)[-1], 'context_window': 65536,
                              'max_output_tokens': 1024}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        type(self).requests.append(body)
        inputs = body.get('input', [])
        functions = [item['name'] for item in body.get('tools', [])]
        rendered = json.dumps(inputs, ensure_ascii=False)
        if 'CUSTOM_CHILD_INSTRUCTION' in rendered:
            text = 'DYNAMIC_CHILD_RESULT'
            output = [{'type': 'message', 'content': [{'type': 'output_text', 'text': text}]}]
        elif 'DYNAMIC_MAIN_INSTRUCTION' in rendered and 'run_tools' in functions:
            if 'handoff_to_helper' in functions and not any(item.get('type') == 'function_call_output' for item in inputs):
                output = [{'type': 'function_call', 'call_id': 'delegate-fixture', 'name': 'handoff_to_helper',
                           'arguments': json.dumps({'task': '整理指定信息'})}]
                text = ''
            else:
                text = 'DYNAMIC_CHILD_RESULT' if 'handoff_to_helper' in functions else 'NO_CHILD_ACTIVE'
                output = [{'type': 'message', 'content': [{'type': 'output_text', 'text': text}]}]
        else:
            # Title generation and on-demand context compaction use the same endpoint.
            text = 'fixture summary'
            output = [{'type': 'message', 'content': [{'type': 'output_text', 'text': text}]}]
        response = {'id': 'fixture-response', 'status': 'completed', 'output': output,
                    'usage': {'input_tokens': 50, 'output_tokens': 10}}
        self.send_response(200)
        if body.get('stream'):
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            if text:
                self.wfile.write(('data: ' + json.dumps({'type': 'response.output_text.delta', 'delta': text}) + '\n\n').encode())
            self.wfile.write(('data: ' + json.dumps({'type': 'response.completed', 'response': response}) + '\n\n').encode())
        else:
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(json.dumps(response).encode())


class AgentSettingsUI(ModuleHost):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        cls.models = ThreadingHTTPServer(('127.0.0.1', 0), ResponsesHandler)
        cls.addClassCleanup(cls.models.server_close)
        cls.addClassCleanup(cls.models.shutdown)
        threading.Thread(target=cls.models.serve_forever, daemon=True).start()
        cls.model_url = f'http://127.0.0.1:{cls.models.server_port}/v1'

    def logged_in(self, viewport=None):
        context = self.browser.new_context(viewport=viewport or {'width': 1280, 'height': 900})
        self.addCleanup(context.close)
        response = context.request.post(self.base + '/auth', headers={'Origin': self.base}, data={'password': 'module-test-password'})
        self.assertTrue(response.ok)
        return context

    def settings(self, context):
        response = context.request.get(self.base + '/api/assistant/agent-settings')
        self.assertTrue(response.ok)
        self.assertEqual(response.headers['cache-control'], 'no-store')
        return response.json()

    def save(self, context, settings):
        response = context.request.put(self.base + '/api/assistant/agent-settings', headers={'Origin': self.base}, data=settings)
        self.assertTrue(response.ok, response.text())
        return response.json()

    def turn(self, context, conversation, message, expected):
        response = context.request.post(self.base + f'/api/assistant/conversations/{conversation}/messages', data={'message': message, 'enable_tools': True})
        self.assertTrue(response.ok, response.text())
        # A very fast run can complete before the stream subscription; persisted
        # history is the authoritative outcome in both cases.
        for _ in range(200):
            history = context.request.get(self.base + f'/api/assistant/conversations/{conversation}/messages')
            self.assertTrue(history.ok, history.text())
            if expected in history.text():
                return history.text()
            time.sleep(0.1)
        self.log.seek(0)
        self.fail('Expected reply did not appear: ' + self.log.read())

    def test_01_page_creation_models_tools_persistence_and_mobile(self):
        context = self.logged_in()
        settings = self.settings(context)
        self.assertEqual(settings['subagents'], [])
        page = context.new_page()
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(self.base)
        page.get_by_role('button', name='设置', exact=True).click()
        page.get_by_role('menuitem', name='Agent 配置', exact=True).click()
        manager = page.get_by_role('dialog', name='Agent 配置', exact=True)
        expect(manager).to_be_visible()
        expect(manager.get_by_label('名称', exact=True)).to_have_value('Firefly')
        manager.get_by_role('button', name='模型配置', exact=True).click()
        manager.get_by_label('服务地址', exact=True).fill(self.model_url)
        manager.get_by_label('模型名称', exact=True).fill('fixture-main')
        manager.get_by_label('API Key', exact=True).fill('private-browser-key')
        manager.get_by_label('输入预算', exact=True).fill('32768')
        manager.get_by_role('button', name='子 agent', exact=False).first.click()
        manager.get_by_role('button', name='＋ 新建子 agent', exact=True).click()
        manager.get_by_label('名称', exact=True).fill('整理助手')
        manager.get_by_label('标识', exact=True).fill('helper')
        manager.get_by_label('职责说明', exact=True).fill('整理指定信息')
        manager.get_by_label('系统提示词', exact=True).fill('CUSTOM_CHILD_INSTRUCTION')
        manager.get_by_label('启用子 agent', exact=True).check()
        manager.locator('.tool-option').filter(has_text='fetch_activity_records').get_by_role('checkbox').check()
        manager.get_by_role('button', name='Firefly 主 agent', exact=True).click()
        manager.get_by_label('系统提示词', exact=True).fill('DYNAMIC_MAIN_INSTRUCTION')
        manager.get_by_role('button', name='保存配置', exact=True).click()
        expect(manager.get_by_role('status')).to_have_text('已保存，从下一轮对话生效。')
        saved = self.settings(context)
        self.assertEqual(saved['subagents'][0]['id'], 'helper')
        self.assertEqual(saved['subagents'][0]['tools'], ['fetch_activity_records'])
        self.assertTrue(saved['profiles'][0]['key_configured'])
        self.assertIsNone(saved['profiles'][0]['api_key'])
        self.assertNotIn('private-browser-key', json.dumps(saved))
        with page.expect_download() as result:
            manager.get_by_role('button', name='导出配置', exact=True).click()
        exported = json.loads(Path(result.value.path()).read_text())
        self.assertIsNone(exported['profiles'][0]['api_key'])
        self.assertNotIn('private-browser-key', json.dumps(exported))
        output = Path('output'); output.mkdir(exist_ok=True)
        page.screenshot(path=str(output / 'agent-settings-desktop.png'), full_page=True)
        manager.get_by_role('button', name='关闭', exact=True).click()
        page.reload()
        page.get_by_role('button', name='设置', exact=True).click()
        page.get_by_role('menuitem', name='Agent 配置', exact=True).click()
        expect(manager.get_by_label('系统提示词', exact=True)).to_have_value('DYNAMIC_MAIN_INSTRUCTION')
        page.set_viewport_size({'width': 390, 'height': 844})
        manager.get_by_role('button', name='子 agent', exact=False).first.click()
        expect(manager.get_by_label('名称', exact=True)).to_have_value('整理助手')
        self.assertLessEqual(manager.evaluate('(element) => element.scrollWidth'), manager.evaluate('(element) => element.clientWidth'))
        page.screenshot(path=str(output / 'agent-settings-mobile.png'), full_page=True)
        self.assertEqual(errors, [])

    def test_02_dynamic_handoff_disable_and_same_conversation_refresh(self):
        context = self.logged_in()
        settings = self.settings(context)
        settings['firefly']['instruction'] = 'DYNAMIC_MAIN_INSTRUCTION'
        settings['profiles'][0].update(base_url=self.model_url, model='fixture-main', context_tokens=32768)
        settings['profiles'] = [settings['profiles'][0], {'id': 'child-model', 'name': '子 agent 模型',
                                'base_url': self.model_url, 'model': 'fixture-child',
                                'api_key': 'private-child-key', 'context_tokens': 32768}]
        settings['subagents'] = [{'id': 'helper', 'name': '整理助手', 'description': '整理指定信息',
                                 'instruction': 'CUSTOM_CHILD_INSTRUCTION', 'model_profile': 'child-model',
                                 'tools': ['fetch_activity_records'], 'enabled': True, 'timeout_secs': 60}]
        self.save(context, settings)
        response = context.request.post(self.base + '/api/assistant/conversations', data={'agent': 'firefly', 'title': '验证动态委派'})
        self.assertTrue(response.ok, response.text())
        conversation = response.json()['id']
        stream = self.turn(context, conversation, '请整理指定信息', 'DYNAMIC_CHILD_RESULT')
        workflow = context.request.get(self.base + f'/api/assistant/conversations/{conversation}/tasks?after=0&tools_after=0&wait_secs=0')
        self.assertTrue(any(task.get('kind') == 'handoff' for task in workflow.json()['tasks']))
        self.assertIn('DYNAMIC_CHILD_RESULT', stream)
        child_requests = [body for body in ResponsesHandler.requests if 'CUSTOM_CHILD_INSTRUCTION' in json.dumps(body.get('input', []))]
        self.assertTrue(child_requests)
        child_tools = [tool['name'] for tool in child_requests[-1]['tools']]
        self.assertIn('fetch_activity_records', child_tools)
        self.assertNotIn('command', child_tools)
        self.assertNotIn('todos', child_tools)
        self.assertEqual(child_requests[-1]['model'], 'fixture-child')
        settings = self.settings(context)
        settings['subagents'][0]['enabled'] = False
        self.save(context, settings)
        start = len(ResponsesHandler.requests)
        stream = self.turn(context, conversation, '再整理一次', 'NO_CHILD_ACTIVE')
        self.assertIn('NO_CHILD_ACTIVE', stream)
        root_requests = [body for body in ResponsesHandler.requests[start:] if body.get('tools')]
        self.assertTrue(root_requests)
        self.assertFalse(any(tool['name'].startswith('handoff_to_') for tool in root_requests[0]['tools']))

    def test_03_auth_origin_stale_revision_and_invalid_configuration(self):
        anonymous = self.browser.new_context()
        self.addCleanup(anonymous.close)
        self.assertEqual(anonymous.request.get(self.base + '/api/assistant/agent-settings').status, 401)
        context = self.logged_in()
        settings = self.settings(context)
        self.assertEqual(context.request.put(self.base + '/api/assistant/agent-settings', headers={'Origin': 'https://unrelated.invalid'}, data=settings).status, 403)
        self.save(context, settings)
        self.assertEqual(context.request.put(self.base + '/api/assistant/agent-settings', headers={'Origin': self.base}, data=settings).status, 409)
        current = self.settings(context)
        current['subagents'] = [{'id': 'firefly', 'name': '重复', 'description': 'invalid', 'instruction': '',
                                'model_profile': None, 'tools': [], 'enabled': True, 'timeout_secs': 60}]
        self.assertEqual(context.request.put(self.base + '/api/assistant/agent-settings', headers={'Origin': self.base}, data=current).status, 400)


if __name__ == '__main__':
    import unittest
    unittest.main()
