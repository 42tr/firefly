"""Verify installed Codex switching with local fake Responses endpoints; no paid model calls.

python3 -m unittest scripts.test_codex_settings_protocol
The synthetic native thread is deleted after the test. No live Firefly APIs are used.
"""
import json
import queue
import shutil
import subprocess
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


@unittest.skipUnless(shutil.which('codex'), 'Codex CLI is not installed')
class CodexSettingsProtocol(unittest.TestCase):
    def setUp(self):
        self.requests = []
        requests = self.requests

        class Responses(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                requests.append(dict(path=self.path, body=body))
                message = dict(id=f'msg_{len(requests)}', type='message', role='assistant', status='completed',
                               content=[dict(type='output_text', text='probe completed', annotations=[])])
                response = dict(id=f'resp_{len(requests)}', object='response', created_at=int(time.time()),
                                status='completed', model=body['model'], output=[message],
                                usage=dict(input_tokens=1, output_tokens=1, total_tokens=2))
                events = [dict(type='response.created', response=dict(response, status='in_progress', output=[])),
                          dict(type='response.output_item.added', output_index=0,
                               item=dict(message, status='in_progress', content=[])),
                          dict(type='response.output_text.delta', item_id=message['id'], output_index=0,
                               content_index=0, delta='probe completed'),
                          dict(type='response.output_item.done', output_index=0, item=message),
                          dict(type='response.completed', response=response)]
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                for event in events:
                    self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())

        self.http = ThreadingHTTPServer(('127.0.0.1', 0), Responses)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        self.addCleanup(self.http.server_close)
        self.addCleanup(self.http.shutdown)
        self.directory = tempfile.TemporaryDirectory(prefix='firefly-codex-settings-')
        self.addCleanup(self.directory.cleanup)
        command = ['codex', 'app-server', '-c', 'thread_unload_delay_secs=0', '-c', 'mcp_servers={}',
                   '-c', 'features.apps=false', '-c', 'features.memories=false',
                   '-c', 'model_provider=firefly_test_a', '-c', 'model=gpt-5.4',
                   '-c', 'model_reasoning_effort=medium']
        for name, suffix in [('firefly_test_a', 'a'), ('firefly_test_b', 'b')]:
            for key, value in dict(name=name, base_url=f'http://127.0.0.1:{self.http.server_port}/{suffix}',
                                   wire_api='responses', requires_openai_auth=False).items():
                command += ['-c', f'model_providers.{name}.{key}={json.dumps(value)}']
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, text=True, cwd=self.directory.name)
        self.addCleanup(self.stop)
        self.output = queue.Queue()
        self.pending = []
        self.request_id = 0
        threading.Thread(target=lambda: [self.output.put(json.loads(line)) for line in self.process.stdout],
                         daemon=True).start()
        self.rpc('initialize', dict(clientInfo=dict(name='firefly_settings_test', version='0.0.0')))
        self.process.stdin.write('{"method":"initialized"}\n')
        self.process.stdin.flush()

    def stop(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.process.stdin.close()
        self.process.stdout.close()

    def rpc(self, method, params):
        self.request_id += 1
        self.process.stdin.write(json.dumps(dict(id=self.request_id, method=method, params=params)) + '\n')
        self.process.stdin.flush()
        deadline = time.monotonic() + 30
        while True:
            event = self.output.get(timeout=max(.01, deadline - time.monotonic()))
            if event.get('id') == self.request_id:
                if 'error' in event:
                    raise AssertionError(event['error'])
                return event['result']
            self.pending.append(event)

    def wait_event(self, method, thread_id, turn_id=None):
        def matches(event):
            params = event.get('params', {})
            return (event.get('method') == method and params.get('threadId') == thread_id
                    and (turn_id is None or params.get('turn', {}).get('id') == turn_id))
        for index, event in enumerate(self.pending):
            if matches(event):
                return self.pending.pop(index)
        deadline = time.monotonic() + 30
        while True:
            event = self.output.get(timeout=max(.01, deadline - time.monotonic()))
            if matches(event):
                return event
            self.pending.append(event)

    def turn(self, thread_id, text, effort):
        result = self.rpc('turn/start', dict(threadId=thread_id, input=[dict(type='text', text=text)], effort=effort))
        completed = self.wait_event('turn/completed', thread_id, result['turn']['id'])
        self.assertEqual(completed['params']['turn']['status'], 'completed')

    def reload(self, thread_id, **settings):
        self.rpc('thread/unsubscribe', dict(threadId=thread_id))
        self.wait_event('thread/closed', thread_id)
        return self.rpc('thread/resume', dict(threadId=thread_id, cwd=self.directory.name, **settings))

    def test_platform_model_effort_history_defaults_and_unrelated_thread(self):
        started = self.rpc('thread/start', dict(cwd=self.directory.name, modelProvider='firefly_test_a', model='gpt-5.4'))
        thread_id = started['thread']['id']
        self.addCleanup(lambda: self.rpc('thread/delete', dict(threadId=thread_id)))
        self.turn(thread_id, 'probe:first. Reply briefly without calling tools.', 'low')
        untouched = self.rpc('thread/start', dict(cwd=self.directory.name, ephemeral=True))['thread']['id']
        ignored = self.rpc('thread/resume', dict(threadId=thread_id, cwd=self.directory.name,
                                              modelProvider='firefly_test_b', model='gpt-5.3-codex'))
        self.assertEqual(ignored['modelProvider'], 'firefly_test_a')
        changed = self.reload(thread_id, modelProvider='firefly_test_b', model='gpt-5.3-codex')
        self.assertEqual(changed['thread']['id'], thread_id)
        self.assertEqual(changed['modelProvider'], 'firefly_test_b')
        self.assertEqual(changed['model'], 'gpt-5.3-codex')
        self.assertIn(untouched, self.rpc('thread/loaded/list', {})['data'])
        self.turn(thread_id, 'probe:second. Reply briefly without calling tools.', 'high')
        defaults = self.rpc('config/read', dict(cwd=self.directory.name, includeLayers=False))['config']
        self.reload(thread_id, modelProvider=defaults['model_provider'], model=defaults['model'])
        self.turn(thread_id, 'probe:default. Reply briefly without calling tools.', defaults['model_reasoning_effort'])
        self.assertEqual([item['path'] for item in self.requests], ['/a/responses', '/b/responses', '/a/responses'])
        self.assertEqual([item['body']['model'] for item in self.requests], ['gpt-5.4', 'gpt-5.3-codex', 'gpt-5.4'])
        self.assertEqual([item['body']['reasoning']['effort'] for item in self.requests], ['low', 'high', 'medium'])
        history = json.dumps(self.requests[-1]['body']['input'])
        self.assertIn('probe:first', history)
        self.assertIn('probe:second', history)


if __name__ == '__main__':
    unittest.main()
