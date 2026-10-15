"""Optional native Claude settings regression against a local mock API.

python3 -m unittest scripts.test_claude_settings_protocol
Requires an installed Claude CLI. Uses isolated settings, fake credentials,
no model tools, and no real inference endpoint.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import unittest


@unittest.skipUnless(shutil.which('claude'), 'Claude CLI is not installed')
class ClaudeSettingsProtocol(unittest.TestCase):
    def test_native_helpers_inline_keys_routing_effort_and_resume(self):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
                if 'count_tokens' in self.path:
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.end_headers()
                    self.wfile.write(b'{"input_tokens":1}')
                    return
                key = self.headers.get('x-api-key') or self.headers.get('Authorization', '').removeprefix('Bearer ')
                seen.append((body, key))
                message = dict(id='msg_local_probe', type='message', role='assistant', model=body['model'], content=[],
                               stop_reason=None, stop_sequence=None, usage=dict(input_tokens=1, output_tokens=1))
                events = [dict(type='message_start', message=message),
                          dict(type='content_block_start', index=0, content_block=dict(type='text', text='')),
                          dict(type='content_block_delta', index=0, delta=dict(type='text_delta', text='local mock response')),
                          dict(type='content_block_stop', index=0),
                          dict(type='message_delta', delta=dict(stop_reason='end_turn', stop_sequence=None), usage=dict(output_tokens=3)),
                          dict(type='message_stop')]
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                for event in events:
                    self.wfile.write(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode())
                self.wfile.flush()

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        with tempfile.TemporaryDirectory(prefix='firefly-native-claude-') as temporary:
            root = Path(temporary)
            (root / '.claude').mkdir()
            (root / '.claude/settings.json').write_text(json.dumps({
                'apiKeyHelper': 'printf old-project-key',
                'env': {'ANTHROPIC_BASE_URL': 'http://127.0.0.1:9', 'ANTHROPIC_API_KEY': 'old-project-key',
                        'ANTHROPIC_AUTH_TOKEN': 'old-project-token', 'CLAUDE_CODE_USE_VERTEX': '1', 'CLAUDE_CODE_EFFORT_LEVEL': 'low'}}))
            environment = {**os.environ, 'CLAUDE_CONFIG_DIR': str(root / 'state'),
                           'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1', 'DISABLE_AUTOUPDATER': '1',
                           'ANTHROPIC_API_KEY': 'old-process-key', 'ANTHROPIC_AUTH_TOKEN': 'old-process-token',
                           'FIREFLY_NATIVE_TEST_KEY': 'helper-test-key'}
            settings_file = root / 'turn.json'
            session = None
            cases = [('claude-opus-4-6', 'max', 'helper-test-key'),
                     ('claude-sonnet-4-6', None, 'rotated-helper-key'),
                     ('claude-sonnet-4-6', None, 'inline-test-key')]
            for index, (model, override, expected_key) in enumerate(cases):
                environment['FIREFLY_NATIVE_TEST_KEY'] = expected_key
                native = {'model': model, 'availableModels': ['claude-opus-4-6', 'claude-sonnet-4-6'],
                          'effortLevel': 'high', 'apiKeyHelper': 'printenv FIREFLY_NATIVE_TEST_KEY',
                          'env': {'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{server.server_port}',
                                  'ANTHROPIC_API_KEY': '', 'ANTHROPIC_AUTH_TOKEN': '', 'CLAUDE_CODE_OAUTH_TOKEN': '',
                                  'CLAUDE_CODE_USE_BEDROCK': '0', 'CLAUDE_CODE_USE_VERTEX': '0', 'CLAUDE_CODE_USE_FOUNDRY': '0',
                                  'CLAUDE_CODE_EFFORT_LEVEL': override or ''}}
                if index == 2:
                    native['apiKeyHelper'] = ''
                    native['env']['ANTHROPIC_API_KEY'] = expected_key
                if index == 0:
                    native['env']['ANTHROPIC_MODEL'] = 'claude-sonnet-4-6'
                settings_file.write_text(json.dumps(native))
                settings_file.chmod(0o600)
                args = ['claude', '--print', '--verbose', '--output-format', 'stream-json', '--include-partial-messages',
                        '--permission-mode', 'dontAsk', '--tools', '', '--strict-mcp-config', '--mcp-config', '{"mcpServers":{}}',
                        '--setting-sources', 'project', '--settings', str(settings_file), '--model', model]
                if override:
                    args += ['--effort', override]
                if session:
                    args += ['--resume', session]
                args += ['--', 'Reply briefly.']
                result = subprocess.run(args, cwd=root, env=environment, capture_output=True, text=True, timeout=60)
                self.assertEqual(result.returncode, 0, result.stderr[-1500:] + result.stdout[-1500:])
                self.assertGreater(len(seen), index)
                body, credential = seen[-1]
                self.assertEqual(credential, expected_key)
                self.assertEqual(body['model'], model)
                self.assertEqual(body.get('output_config', {}).get('effort'), override or 'high')
                messages = [json.loads(line) for line in result.stdout.splitlines() if line.strip().startswith('{')]
                returned_session = next(message['session_id'] for message in messages if message.get('type') == 'result')
                if session:
                    self.assertEqual(returned_session, session)
                session = returned_session


if __name__ == '__main__':
    unittest.main()
