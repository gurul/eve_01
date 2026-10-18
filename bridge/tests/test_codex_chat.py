"""Exercise new-chat lifecycle over JSON lines and folder selection independently."""
import asyncio
import json
from unittest.mock import patch

import pytest
from test_codex_computer import Process, browser_result

from eve_bridge.codex_chat import (
    CodexChat,
    CodexUnavailable,
    accessible_folders,
    folder_menu,
    select_folder,
)


class ChatProcess(Process):
    def __init__(self, **kw):
        super().__init__(**kw)
        self.turn_count = 0

    def write(self, data):
        message = json.loads(data)
        if message.get('method') == 'thread/start':
            self.requests.append(message)
            self.out({'id': message['id'], 'result': {'thread': {
                'id': 'thread', 'cwd': message['params']['cwd']}}})
        else:
            if message.get('method') == 'turn/start':
                self.turn_count += 1
            super().write(data)


def test_accessible_folders_come_from_saved_projects_not_tasks(tmp_path):
    first, other = tmp_path / 'eve', tmp_path / 'My Notes'
    first.mkdir()
    other.mkdir()
    state = {'local-projects': {'a': {'rootPaths': [str(first), str(other)]},
                                'b': {'rootPaths': [str(first), '/no/such/folder', 'relative']}},
             'thread-project-assignments': {'guardian': '/private/internal'}}
    catalog = tmp_path / '.codex-global-state.json'
    catalog.write_text(json.dumps(state))
    before = catalog.read_bytes()
    assert accessible_folders(tmp_path) == [first, other]
    assert folder_menu(accessible_folders(tmp_path)) == 'eve\nMy Notes'
    assert catalog.read_bytes() == before
    for value in ['eve', 'EVE', str(first)+'/', '"'+str(first)+'"']:
        assert select_folder([first, other], value) == first
    duplicate = tmp_path / 'elsewhere' / 'eve'
    with pytest.raises(CodexUnavailable, match='full path'):
        select_folder([first, duplicate], 'eve')
    assert folder_menu([first, duplicate]) == str(first)+'\n'+str(duplicate)
    with pytest.raises(CodexUnavailable, match='not available'):
        select_folder([first], '1')


def test_new_thread_reused_for_followups_and_recreated_on_selection(tmp_path):
    async def go():
        processes, replies = [], []
        async def spawn(*args, **kw):
            assert 'CODEX_APP_TOOLS_PIPE_PATH' not in kw['env']
            processes.append(ChatProcess())
            return processes[-1]
        async def emit(text): replies.append(text)
        async def ask(q): raise AssertionError(q)
        chat = CodexChat(ask_user=ask)
        with patch('asyncio.create_subprocess_exec', spawn):
            await chat.start(tmp_path, emit)
            assert chat.connected and not chat.running
            await chat.send('first prompt')
            await chat._turn_job
            await chat.send('follow up')
            await chat._turn_job
            requests = processes[0].requests
            start = next(m['params'] for m in requests if m.get('method') == 'thread/start')
            assert start['ephemeral'] is False and start['cwd'] == str(tmp_path)
            assert start['sandbox'] == 'workspace-write' and start['approvalPolicy'] == 'on-request'
            assert 'model' not in start
            turns = [m['params'] for m in requests if m.get('method') == 'turn/start']
            assert len(turns) == 2 and all(t['threadId'] == 'thread' for t in turns)
            assert replies.count('Calculator displays 4.') == 2
            assert not any('UI-state' in text for text in replies)  # coding/chat need no UI proof
            await chat.start(tmp_path, emit)
            assert len(processes) == 2 and processes[0].returncode == 0
            assert not any(m.get('method') == 'thread/resume' for p in processes for m in p.requests)
            await chat.close()
            assert not chat.connected and processes[-1].returncode == 0
    asyncio.run(go())


def test_active_followup_steers_and_stop_keeps_chat_usable(tmp_path):
    async def go():
        p, replies = ChatProcess(mode='hold'), []
        async def spawn(*a, **kw): return p
        async def emit(text): replies.append(text)
        async def ask(q): return 'no'
        chat = CodexChat(ask_user=ask)
        with patch('asyncio.create_subprocess_exec', spawn):
            await chat.start(tmp_path, emit)
            await chat.send('work')
            await chat.send('correction')
            assert any(m.get('method') == 'turn/steer' for m in p.requests)
            await chat.interrupt()
            await chat._turn_job
            assert chat.connected and not chat.running and 'Codex stopped.' in replies
            await chat.send('next')
            await chat.close()
            assert sum(m.get('method') == 'turn/interrupt' for m in p.requests) >= 2
            assert not chat.running
    asyncio.run(go())


def test_missing_folder_and_disconnection_never_retry(tmp_path):
    async def go():
        p = ChatProcess(mode='disconnect')
        async def spawn(*a, **kw): return p
        async def emit(text): pass
        async def ask(q): return 'no'
        chat = CodexChat(ask_user=ask, rpc_timeout=.1)
        with patch('asyncio.create_subprocess_exec', spawn):
            with pytest.raises(CodexUnavailable, match='accessible'):
                await chat.start(tmp_path / 'missing', emit)
            # Allow bootstrap responses, then drop at turn/start.
            p.mode = 'hold'
            await chat.start(tmp_path, emit)
            p.mode = 'disconnect'
            with pytest.raises(CodexUnavailable, match='not retried'):
                await chat.send('work')
            assert not chat.connected
            assert sum(m.get('method') == 'turn/start' for m in p.requests) == 1
            await chat.close()
    asyncio.run(go())


def test_browser_capture_callback_uses_current_chat_result(tmp_path):
    async def go():
        p, pictures = ChatProcess(mode='hold'), []
        async def spawn(*a, **kw): return p
        async def emit(text): pass
        async def ask(q): return 'no'
        chat = CodexChat(ask_user=ask)
        async def picture(): pictures.append(chat.browser_screenshot.data)
        with patch('asyncio.create_subprocess_exec', spawn):
            await chat.start(tmp_path, emit, picture)
            await chat.send('browse')
            p.event('item/completed', item={'id': 'capture', 'type': 'mcpToolCall', 'server': 'cua_repl',
                    'tool': 'js', 'status': 'completed', 'result': browser_result()})
            p.event('turn/completed', turn={'id': 'turn', 'status': 'completed'})
            await chat._turn_job
            assert len(pictures) == 1 and pictures[0].startswith(b'\x89PNG')
            await chat.send('ordinary question')
            assert chat.browser_screenshot is None and not chat.browser_used
            await chat.close()
    asyncio.run(go())


@pytest.mark.parametrize('answer,decision', [('yes', 'accept'), ('yes but not yet', 'decline'), ('no', 'decline')])
def test_command_permission_has_exact_scope(answer, decision):
    async def go():
        questions, responses = [], []
        async def ask(q):
            questions.append(q)
            return answer
        async def send(m): responses.append(m)
        chat = CodexChat(ask_user=ask)
        chat.thread_id = 'thread'
        chat._send = send
        await chat._answer_request({'id': 123, 'method': 'item/commandExecution/requestApproval',
                                   'params': {'threadId': 'thread', 'command': 'git status', 'cwd': '/project'}})
        assert questions and 'git status' in questions[0] and '/project' in questions[0]
        assert responses == [{'id': 123, 'result': {'decision': decision}}]
    asyncio.run(go())


def test_a_broken_question_still_answers_the_approval_with_decline():
    """Only OSError and TimeoutError were caught: any other failure to ask left Codex waiting on the approval
    until max_secs, with no decision sent."""
    async def go():
        responses = []
        async def ask(q):
            raise RuntimeError('telegram said 400')
        async def send(m): responses.append(m)
        chat = CodexChat(ask_user=ask)
        chat.thread_id = 'thread'
        chat._send = send
        await chat._answer_request({'id': 7, 'method': 'item/commandExecution/requestApproval',
                                   'params': {'threadId': 'thread', 'command': 'rm -rf build', 'cwd': '/project'}})
        assert responses == [{'id': 7, 'result': {'decision': 'decline'}}]
    asyncio.run(go())


def test_stop_clears_a_pending_permission_question(tmp_path):
    async def go():
        p = ChatProcess(mode='approval')
        asking, cancelled = asyncio.Event(), asyncio.Event()
        async def spawn(*a, **kw): return p
        async def emit(text): pass
        async def ask(q):
            asking.set()
            try:
                await asyncio.Future()
            finally:
                cancelled.set()
        chat = CodexChat(ask_user=ask)
        with patch('asyncio.create_subprocess_exec', spawn):
            await chat.start(tmp_path, emit)
            await chat.send('work')
            await asking.wait()
            await chat.interrupt()
            await chat._turn_job
            assert cancelled.is_set() and chat.connected and not chat.running
            assert not any(m.get('id') == 100 for m in p.requests)
            await chat.close()
    asyncio.run(go())


def test_the_turn_result_goes_to_done_when_the_caller_tells_it_from_progress(tmp_path):
    async def go():
        processes, steps, results = [], [], []
        async def spawn(*args, **kw):
            processes.append(ChatProcess())
            return processes[-1]
        async def emit(text): steps.append(text)
        async def done(text): results.append(text)
        async def ask(q): raise AssertionError(q)
        chat = CodexChat(ask_user=ask)
        with patch('asyncio.create_subprocess_exec', spawn):
            await chat.start(tmp_path, emit, done=done)
            await chat.send('first prompt')
            await chat._turn_job
            assert results == ['Calculator displays 4.']
            assert 'Calculator displays 4.' not in steps       # the result is not also a step
            await chat.close()
    asyncio.run(go())


def test_a_step_emitted_just_before_the_turn_ends_arrives_before_the_result():
    """Review finding: a step's delivery was a background job that _finish_turn cancelled (the step was lost)
    or left running (it arrived after the result)."""
    from eve_bridge.agent_contract import AgentEvent

    async def go():
        order = []

        async def emit(text):
            await asyncio.sleep(0)
            await asyncio.sleep(0)
            order.append(('step', text))

        async def done(text):
            order.append(('result', text))

        async def ask(q):
            raise AssertionError(q)

        chat = CodexChat(ask_user=ask)
        chat._output, chat._result = emit, done
        chat.final = 'All done.'
        chat._done = asyncio.get_running_loop().create_future()
        chat._event(AgentEvent('progress', 'Running the tests'))
        chat._done.set_result({'status': 'completed'})
        await chat._finish_turn()
        assert order == [('step', 'Running the tests'), ('result', 'All done.')]

    asyncio.run(go())
