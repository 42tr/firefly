"""Streaming/layout regression against the real app with isolated mock APIs.

Start Vite, then set FIREFLY_TEST_URL (default http://127.0.0.1:5297/).
No backend or model credentials are used.
"""
import json
import os
import unittest
from urllib.parse import urlparse

from playwright.sync_api import expect, sync_playwright


class StreamingUI(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        playwright = sync_playwright().start()
        cls.addClassCleanup(playwright.stop)
        cls.browser = playwright.chromium.launch(headless=True)
        cls.addClassCleanup(cls.browser.close)

    def exercise(self, width, height, ending='done'):
        page = self.browser.new_page(viewport={'width': width, 'height': height})
        self.addCleanup(page.context.close)
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.add_init_script('''
          window.WebSocket = class { readyState = 0; send() {} close() {} };
          const nativeFetch = window.fetch;
          window.fetch = (input, init = {}) => {
            const url = typeof input === 'string' ? input : input.url;
            if (/\\/api\\/assistant\\/jobs\\/job\\/stream/.test(url)) {
              const body = new ReadableStream({ start(controller) {
                let seq = 0;
                window.emitStream = (event, data = {}) => controller.enqueue(new TextEncoder().encode(
                  'event: ' + event + '\\ndata: ' + JSON.stringify({seq: ++seq, ...data}) + '\\n\\n'));
                init.signal?.addEventListener('abort', () => controller.error(new DOMException('Aborted', 'AbortError')));
              }});
              return Promise.resolve(new Response(body, {headers: {'Content-Type': 'text/event-stream'}}));
            }
            return nativeFetch(input, init);
          };
          Object.defineProperty(navigator, 'clipboard', {value: {writeText: async () => {}}});
        ''')
        active = True
        records = [dict(id=1, conversation_id=1, role='user', content='检查流式界面',
                        generation_job_id='job', processing_status='running')]
        row = dict(id=1, title='流式回归', agent='firefly', status='running',
                   workspace_mode='shared', workspace_state='ready', is_archived=False)

        def api(route):
            path = urlparse(route.request.url).path
            data = {}
            if path == '/api/assistant/conversations':
                data = {'conversations': [row]}
            elif path.endswith('/messages'):
                data = {'messages': records, 'has_more': False}
            elif path.endswith('/active-generation'):
                data = {'active': active, 'job_id': 'job' if active else None}
            elif path.endswith('/process'):
                data = {'messages': records}
            elif path.endswith('/read'):
                data = {'read_message_id': 1}
            route.fulfill(content_type='application/json', body=json.dumps(data))

        page.route('**/api/**', api)
        image_requests = []

        def image(route):
            image_requests.append(route.request.url)
            route.fulfill(content_type='image/svg+xml', body='<svg xmlns="http://www.w3.org/2000/svg" width="240" height="80"><rect width="240" height="80" fill="#d5d9e4"/></svg>')

        page.route('https://example.test/image.svg', image)
        base = os.getenv('FIREFLY_TEST_URL', 'http://127.0.0.1:5297/').rstrip('/')
        page.goto(base + '/?conversation=1')
        page.wait_for_function('typeof window.emitStream === "function"')

        def emit(event, **data):
            page.evaluate('([event, data]) => window.emitStream(event, data)', [event, data])

        emit('stream_started', job_id='job')
        opening = '稳定段落：[报告][report]\n\n```js\n' + 'const longValue = "' + 'value' * 100 + '";'
        emit('assistant_delta', delta=opening)
        code = page.locator('.message.assistant pre')
        expect(code).to_have_count(1)
        page.evaluate('''() => {
          const body = document.querySelector('.message.assistant .markdown-body');
          window.saved = {body, paragraph: body.querySelector('p'), pre: body.querySelector('pre')};
          saved.pre.scrollLeft = 150;
        }''')
        closing = '\n```\n\n![图示](https://example.test/image.svg)\n\n连续输出开始。'
        emit('assistant_delta', delta=closing)
        expect(page.locator('.message.assistant img')).to_be_visible()
        page.wait_for_function('document.querySelector(".message.assistant img").complete')
        self.assertTrue(page.evaluate('saved.pre === document.querySelector(".message.assistant pre") && saved.pre.scrollLeft === 150'))
        page.evaluate('saved.image = document.querySelector(".message.assistant img")')
        page.locator('.message.assistant .code-copy-btn').click()
        expect(page.locator('.code-copy-btn')).to_have_text('已复制')
        emit('assistant_delta', delta='追加内容。')
        expect(page.locator('.message.assistant')).to_contain_text('追加内容。')
        expect(page.locator('.code-copy-btn')).to_have_text('已复制')

        # Finish the reference definition after the paragraph has been displayed.
        emit('assistant_delta', delta='\n\n[report]: https://example.test/report\n\n')
        expect(page.locator('.message.assistant a')).to_have_attribute('href', 'https://example.test/report')
        expect(page.locator('.message.assistant a')).to_have_attribute('rel', 'noopener noreferrer')
        emit('assistant_delta', delta='<img src="x" onerror="window.unsafe = true"><script>window.unsafe = true</script>\n\n')
        page.wait_for_timeout(160)
        self.assertFalse(page.evaluate('!!window.unsafe'))
        self.assertEqual(page.locator('.markdown-body [onerror], .markdown-body script').count(), 0)

        # Record actual per-frame scrolling while 80 independent SSE chunks arrive.
        metrics = page.evaluate('''async () => {
          const panel = document.querySelector('.chat-panel');
          const samples = [];
          let recording = true;
          const record = () => { samples.push(panel.scrollTop); if (recording) requestAnimationFrame(record); };
          requestAnimationFrame(record);
          for (let i = 0; i < 80; i++) {
            emitStream('assistant_delta', {delta: '测试连续流式文本输出。'});
            await new Promise(resolve => setTimeout(resolve, 15));
          }
          await new Promise(resolve => setTimeout(resolve, 350));
          recording = false;
          const steps = samples.slice(1).map((top, i) => top - samples[i]);
          return {maxStep: Math.max(...steps), backwards: steps.filter(step => step < -1).length,
            bottomGap: panel.scrollHeight - panel.clientHeight - panel.scrollTop};
        }''')
        self.assertEqual(metrics['backwards'], 0)
        self.assertLessEqual(metrics['bottomGap'], 1)
        self.assertTrue(page.evaluate('saved.paragraph === saved.body.querySelector("p") && saved.pre === saved.body.querySelector("pre") && saved.image === saved.body.querySelector("img")'))
        self.assertEqual(len(image_requests), 1)

        panel = page.locator('.chat-panel')
        box = panel.bounding_box()
        page.mouse.move(box['x'] + box['width'] / 2, box['y'] + box['height'] / 2)
        page.mouse.wheel(0, -180)
        expect(page.get_by_role('button', name='回到底部', exact=True)).to_be_visible()
        page.wait_for_timeout(150)
        reading_top = panel.evaluate('(node) => node.scrollTop')
        emit('assistant_delta', delta='继续输出而不打断阅读。' * 20)
        expect(page.locator('.message.assistant')).to_contain_text('继续输出而不打断阅读。')
        self.assertAlmostEqual(panel.evaluate('(node) => node.scrollTop'), reading_top, delta=1)
        page.get_by_role('button', name='回到底部', exact=True).click()
        self.assertLessEqual(panel.evaluate('(node) => node.scrollHeight - node.clientHeight - node.scrollTop'), 1)

        before_tool_height = panel.evaluate('(node) => node.scrollHeight')
        emit('tool_call', id='call', name='command', arguments={'command': 'example'})
        expect(page.locator('.process-group')).to_have_count(1)
        expect(page.locator('.message.assistant')).to_have_count(1)
        emit('tool_result', id='call', name='command', result='done')
        emit('assistant_delta', delta='最终回答。\n\n| 列一 | 列二 |\n')
        expect(page.locator('.message.assistant')).to_have_count(2)
        emit('assistant_delta', delta='| --- | --- |\n| 内容 | 内容 |\n')
        expect(page.locator('.message.assistant table')).to_have_count(1)
        self.assertGreaterEqual(panel.evaluate('(node) => node.scrollHeight'), before_tool_height)
        self.assertTrue(page.evaluate('saved.body.isConnected'))
        page.locator('.process-group-header').click()
        expect(page.locator('.process-group-body')).to_be_visible()

        active_height = panel.evaluate('(node) => node.clientHeight')
        active = False
        row['status'] = 'completed'
        if ending == 'stop':
            page.get_by_role('button', name='停止当前回复', exact=True).click()
        elif ending == 'recovery':
            # Replace message objects with the committed snapshot, as happens
            # when a replay log expires. Previously visible notes stay outside.
            records[0]['processing_status'] = 'completed'
            records.extend([
                dict(id=2, conversation_id=1, role='assistant', content=opening + closing,
                     generation_job_id='job', processing_status='completed'),
                dict(id=3, conversation_id=1, role='tool', tool_call_id='call',
                     content='done', metadata={'tool_name': 'command'}, generation_job_id='job'),
                dict(id=4, conversation_id=1, role='assistant', content='最终回答。',
                     generation_job_id='job', processing_status='completed'),
            ])
            emit('stream_reset', reason='log_expired')
        else:
            emit('assistant_done')
        expect(page.locator('.input-status')).to_have_class('input-status input-status-idle')
        self.assertEqual(panel.evaluate('(node) => node.clientHeight'), active_height)
        expect(page.locator('.message.assistant')).to_have_count(2)
        page.get_by_role('button', name='收起中间过程', exact=True).click()
        expect(page.locator('.message.assistant')).to_have_count(1)
        expect(page.locator('.process-group-body')).to_have_count(0)
        expect(page.locator('.message.assistant')).to_contain_text('最终回答。')
        page.locator('.process-group-header').click()
        expect(page.locator('.process-step-fragment')).to_contain_text('稳定段落：')
        self.assertLessEqual(page.evaluate('document.documentElement.scrollWidth'), width)
        self.assertEqual(errors, [])
        print(f'{width}px streaming metrics: {metrics}')

    def test_desktop(self):
        self.exercise(1280, 800)

    def test_mobile(self):
        self.exercise(390, 844)

    def test_stop(self):
        self.exercise(1280, 800, ending='stop')

    def test_snapshot_recovery(self):
        self.exercise(1280, 800, ending='recovery')


if __name__ == '__main__':
    unittest.main()
