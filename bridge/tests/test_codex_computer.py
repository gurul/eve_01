"""Protocol contract tests; a separate recorded native-app check proves the real plugin."""
import asyncio
import base64
import io
from types import SimpleNamespace
from unittest.mock import patch

from PIL import Image

from eve_bridge.codex_computer import CodexComputerAgent, external_environment
from eve_bridge.daemon import Daemon


class Process:
    def __init__(self, *, available=True, mode='complete', answer_schema=None):
        self.stdout = asyncio.StreamReader(limit=32*1024*1024)
        self.stdin = self
        self.returncode = None
        self.requests = []
        self.available, self.mode = available, mode
        self.answer_schema = answer_schema or {'type': 'object', 'properties': {}}
        self.started = asyncio.Event()
        self.finished = asyncio.Event()

    def out(self, obj):
        import json
        self.stdout.feed_data((json.dumps(obj)+'\n').encode())

    def event(self, method, **params):
        self.out({'method': method, 'params': {'threadId': 'thread', **params}})

    def write(self, data):
        import json
        m = json.loads(data)
        self.requests.append(m)
        method = m.get('method')
        result = {}
        if method == 'initialize':
            result = {'userAgent': 'test'}
        elif method == 'thread/start':
            result = {'thread': {'id': 'thread'}}
        elif method == 'mcpServerStatus/list':
            if not m['params'].get('cursor'):
                result = {'data': [{'name': 'other', 'tools': {'js': {}}}], 'nextCursor': 'page2'}
            else:
                result = {'data': [{'name': 'cua_repl', 'tools': {'js': {}}}] if self.available else [], 'nextCursor': None}
        elif method == 'turn/start':
            result = {'turn': {'id': 'turn'}}
            self.started.set()
            self.event('item/completed', item={'type': 'agentMessage', 'phase': 'commentary', 'text': 'Opening Calculator'})
            if self.mode == 'approval':
                self.out({'id': 100, 'method': 'mcpServer/elicitation/request', 'params': {
                    'threadId': 'thread', 'serverName': 'cua_repl', 'mode': 'form',
                    'message': 'Allow Computer Use to use "Calculator"?', 'requestedSchema': self.answer_schema}})
            elif self.mode == 'disconnect':
                self.stdout.feed_eof()
            elif self.mode == 'complete':
                self.complete()
        elif method == 'turn/interrupt':
            self.event('turn/completed', turn={'id': 'turn', 'status': 'interrupted'})
        elif method == 'turn/steer':
            result = {'turnId': 'turn'}
        elif m.get('id') == 100:
            self.complete()
        if method and 'id' in m and self.mode != 'disconnect':
            self.out({'id': m['id'], 'result': result})

    def complete(self):
        self.event('item/completed', item={'id': 'cua-call', 'type': 'mcpToolCall', 'server': 'cua_repl',
            'tool': 'js', 'status': 'completed', 'result': {'content': [{'type': 'text', 'text': 'Window: Calculator. Value: 4'}]}})
        self.event('item/completed', item={'type': 'agentMessage', 'phase': 'final_answer', 'text': 'Calculator displays 4.'})
        self.event('turn/completed', turn={'id': 'turn', 'status': 'completed'})

    async def drain(self):
        pass

    def terminate(self):
        self.returncode = 0
        if not self.stdout.at_eof():
            self.stdout.feed_eof()
        self.finished.set()

    kill = terminate

    async def wait(self):
        await self.finished.wait()
        return self.returncode


def test_inventory_pagination_progress_result_and_no_credentials():
    async def go():
        p = Process()
        events = []
        async def spawn(*args, **kw):
            assert args[1:] == ('app-server', '--listen', 'stdio://')
            assert 'CODEX_APP_TOOLS_PIPE_PATH' not in kw['env']
            return p
        async def ask(q):
            raise AssertionError(q)
        agent = CodexComputerAgent(on_event=events.append, ask_user=ask)
        with patch('asyncio.create_subprocess_exec', spawn):
            assert await agent.run('Compute 2+2') == 'Calculator displays 4.'
        assert not agent.running and agent.ui_evidence[0]['state'].endswith('4')
        assert any(e.kind == 'progress' and e.text == 'Opening Calculator' for e in events)
        assert any(e.kind == 'final' for e in events)
        start = next(m['params'] for m in p.requests if m.get('method') == 'thread/start')
        assert start['ephemeral'] and start['approvalPolicy'] == 'on-request' and start['sandbox'] == 'read-only'
        assert 'model' not in start
    asyncio.run(go())
    with patch.dict('os.environ', {'CODEX_APP_TOOLS_PIPE_PATH': 'secret', 'OPENAI_API_KEY': 'secret', 'HOME': '/tmp/home'}):
        assert 'OPENAI_API_KEY' not in external_environment()
        assert external_environment()['HOME'] == '/tmp/home'


def test_missing_tool_never_starts_a_turn_positive_control_in_previous_test():
    async def go():
        p = Process(available=False)
        async def spawn(*a, **kw): return p
        async def ask(q): raise AssertionError(q)
        agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask)
        with patch('asyncio.create_subprocess_exec', spawn):
            assert 'no cua_repl.js' in await agent.run('do something')
        assert not any(m.get('method') == 'turn/start' for m in p.requests)
    asyncio.run(go())


def test_approvals_are_once_explicit_and_unknown_forms_fail_closed():
    async def go(answer, schema=None):
        p = Process(mode='approval', answer_schema=schema)
        questions = []
        async def spawn(*a, **kw): return p
        async def ask(q):
            questions.append(q)
            return answer
        agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask)
        with patch('asyncio.create_subprocess_exec', spawn):
            await agent.run('Compute 2+2')
        reply = next(m for m in p.requests if m.get('id') == 100)
        assert '_meta' not in reply.get('result', {})  # no saved/always permission
        return reply, questions
    for answer, expected in [('yes', 'accept'), ('no', 'decline'), ('yes but not yet', 'decline'), ('', 'decline')]:
        reply, questions = asyncio.run(go(answer))
        assert reply['result']['action'] == expected
        assert questions and 'Calculator' in questions[0]
    reply, questions = asyncio.run(go('yes', {'type': 'object', 'properties': {'secret': {'type': 'string'}}}))
    assert reply['result']['action'] == 'decline' and not questions


def test_cancel_while_approval_waits_interrupts_turn_and_cleans_up():
    async def go():
        p = Process(mode='approval')
        asking = asyncio.Event()
        async def spawn(*a, **kw): return p
        async def ask(q):
            asking.set()
            await asyncio.Future()
        agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask)
        with patch('asyncio.create_subprocess_exec', spawn):
            run = asyncio.create_task(agent.run('Compute 2+2'))
            await asking.wait()
            agent.cancel()
            assert 'stopped' in await asyncio.wait_for(run, 1)
        assert any(m.get('method') == 'turn/interrupt' for m in p.requests)
        assert not any(m.get('id') == 100 for m in p.requests)
        assert p.returncode == 0 and not agent.running
    asyncio.run(go())


def test_steering_and_disconnect():
    async def go():
        p = Process(mode='hold')
        async def spawn(*a, **kw): return p
        async def ask(q): return 'no'
        agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask, rpc_timeout=.2)
        with patch('asyncio.create_subprocess_exec', spawn):
            run = asyncio.create_task(agent.run('test'))
            await p.started.wait()
            for _ in range(10):
                if agent.turn_id:
                    break
                await asyncio.sleep(0)
            assert agent.steer('Use 3+3 instead')
            await asyncio.sleep(.01)
            assert any(m.get('method') == 'turn/steer' for m in p.requests)
            p.stdout.feed_eof()
            assert 'disconnected' in await asyncio.wait_for(run, 1)
    asyncio.run(go())


def test_daemon_factory_routes_shared_task_contract_to_codex():
    host = SimpleNamespace()
    agent = Daemon._make_agent(host, lambda e: None, lambda q: None)
    # Codex behind the launch reflex (app_reflex.py): built on handoff, with Codex's contract.
    assert agent.provider == 'codex' and agent._inner is None
    assert isinstance(agent._make_inner(), CodexComputerAgent)
    assert host._active_agent is agent


def browser_result(tab_id='7', state_id='7', data=None):
    out = io.BytesIO()
    Image.new('RGB', (16, 12), 'blue').save(out, format='PNG')
    return {'content': [
        {'type': 'text', 'text': f'EVE_BROWSER_CAPTURE {{"tabId":"{tab_id}"}}'},
        {'type': 'text', 'text': f'Browser tab: {state_id}, Title: "Example", URL: "https://example.com/".\n0 AXWebArea Example'},
        {'type': 'image', 'mimeType': 'image/png', 'data': data or base64.b64encode(out.getvalue()).decode()},
    ]}


def notify_browser(agent, result, thread='thread'):
    agent._notification({'method': 'item/completed', 'params': {'threadId': thread, 'item': {
        'id': 'capture', 'type': 'mcpToolCall', 'server': 'cua_repl', 'tool': 'js',
        'status': 'completed', 'result': result}}})


def test_browser_image_is_bound_to_the_observed_task_tab_and_verified_bytes():
    agent = CodexComputerAgent(on_event=lambda e: None, ask_user=lambda q: None)
    agent.thread_id = 'thread'
    notify_browser(agent, browser_result(), thread='other')
    assert not agent.browser_used and agent.browser_screenshot is None
    notify_browser(agent, browser_result())
    assert agent.browser_used and agent.browser_tab_id == '7'
    assert agent.browser_screenshot.mime == 'image/png'
    assert agent.ui_evidence and 'Browser tab: 7,' in agent.ui_evidence[-1]['state']
    with Image.open(io.BytesIO(agent.browser_screenshot.data)) as im:
        assert im.size == (16, 12) and im.getpixel((0, 0)) == (0, 0, 255)
    for bad in (browser_result(state_id='8'), browser_result(data='invalid!'),
                browser_result(data=base64.b64encode(b'not an image').decode()),
                {**browser_result(), 'isError': True},
                {'content': browser_result()['content'][1:]},
                {'content': browser_result()['content'][:1] + browser_result()['content'][2:]}):
        notify_browser(agent, browser_result())  # a known good image must be invalidated
        assert agent.browser_screenshot is not None
        notify_browser(agent, bad)
        assert agent.browser_screenshot is None


def test_new_cua_action_invalidates_previous_browser_picture():
    agent = CodexComputerAgent(on_event=lambda e: None, ask_user=lambda q: None)
    agent.thread_id = 'thread'
    notify_browser(agent, browser_result())
    agent._notification({'method': 'item/started', 'params': {'threadId': 'thread',
        'item': {'type': 'mcpToolCall', 'server': 'cua_repl', 'tool': 'js'}}})
    assert agent.browser_screenshot is None and agent.browser_used
