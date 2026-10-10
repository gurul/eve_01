"""Real agent lifecycle over fake app-server JSON lines; spend is read from the temporary ledger.

Shapes verified with the bundled Codex app-server generate-ts on 2026-09-28:
ThreadStartResponse.model and ThreadTokenUsageUpdatedNotification.tokenUsage.total.
`last` describes one model request, not a whole turn; reasoning is already in outputTokens.
"""
from __future__ import annotations

import asyncio
import json
from datetime import date

import pytest
from test_codex_computer import Process

from eve_bridge import codex_auth, spend
from eve_bridge.codex_chat import CodexChat
from eve_bridge.codex_computer import CodexComputerAgent, external_environment
from eve_bridge.codex_warm import WarmCodex


def rows(folder):
    return spend.day_rows(date.today().isoformat(), folder)


def usage(inp, cached=0, out=0):
    return {'inputTokens': inp, 'cachedInputTokens': cached, 'outputTokens': out,
            'totalTokens': inp + out, 'reasoningOutputTokens': out, 'cacheWriteInputTokens': 0}


class MeterProcess(Process):
    def __init__(self, *, totals=(), model='gpt-6-sol', hold=False, interrupt_total=None, interrupt_completed=True):
        super().__init__()
        self.totals, self.model, self.hold = totals, model, hold
        self.interrupt_total = interrupt_total
        self.interrupt_completed = interrupt_completed
        self.turn = 0

    @property
    def turn_id(self):
        return f'turn-{self.turn}'

    def token_event(self, total, *, turn_id=None, thread_id='thread'):
        self.event('thread/tokenUsage/updated', threadId=thread_id, turnId=turn_id or self.turn_id,
                   tokenUsage={'total': total, 'last': usage(1), 'modelContextWindow': 100000})

    def finish(self, status='completed'):
        self.event('item/completed', item={'type': 'agentMessage', 'phase': 'final_answer', 'text': 'Done.'})
        self.event('turn/completed', turn={'id': self.turn_id, 'status': status})

    def write(self, data):
        msg = json.loads(data)
        method = msg.get('method')
        if method not in {'thread/start', 'turn/start', 'turn/interrupt'}:
            return super().write(data)
        self.requests.append(msg)
        if method == 'thread/start':
            result = {'thread': {'id': 'thread', 'cwd': msg['params']['cwd']}, 'model': self.model}
        elif method == 'turn/start':
            self.turn += 1
            result = {'turn': {'id': self.turn_id}}
            # Notifications may arrive before the matching RPC result.
            self.event('turn/started', turn={'id': self.turn_id})
            for total in self.totals[self.turn - 1] if self.turn <= len(self.totals) else ():
                self.token_event(total)
            self.started.set()
            if not self.hold:
                self.finish()
        else:
            result = {}
            if self.interrupt_total is not None:
                self.token_event(self.interrupt_total)
            if self.interrupt_completed:
                self.finish('interrupted')
        self.out({'id': msg['id'], 'result': result})


def wire(monkeypatch, process, tmp_path, mode='api'):
    env = {'HOME': str(tmp_path), 'CODEX_HOME': str(tmp_path / 'isolated-codex')}
    monkeypatch.setattr(codex_auth, 'launch_environment', lambda: (env.copy(), mode))
    launches = []

    async def spawn(*args, **kw):
        launches.append((args, kw['env']))
        return process

    monkeypatch.setattr(asyncio, 'create_subprocess_exec', spawn)
    return env, launches


async def ask(_question):
    return 'no'


async def emit(_text):
    pass


def test_api_computer_uses_process_home_and_prices_full_turn_once(monkeypatch, tmp_path, _spend_ledger_in_tmp):
    async def go():
        p = MeterProcess(totals=[[usage(1000, 400, 100), usage(2000, 800, 300), usage(2000, 800, 300)]])
        env, launches = wire(monkeypatch, p, tmp_path)
        assert external_environment() == env
        agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask)
        await agent.run('inspect')
        assert len(launches) == 1 and launches[0][1] == env
        assert all('model' not in m.get('params', {}) for m in p.requests)
    asyncio.run(go())
    got = rows(_spend_ledger_in_tmp)
    assert len(got) == 1
    assert got[0] | {'t': 0} == {
        't': 0, 'p': 'openai', 'm': 'codex', 'f': 'codex', 'usd': pytest.approx(.00556),
        'tok': {'in': 2000, 'cached': 800, 'out': 300}, 'src': 'priced',
        'note': 'API key, billed per token (computer task)',
    }


def test_plan_metering_stays_unpriced_and_is_recorded_before_turn(monkeypatch, tmp_path, _spend_ledger_in_tmp):
    async def go():
        p = MeterProcess(totals=[[usage(1000)]], hold=True)
        wire(monkeypatch, p, tmp_path, 'plan')
        agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask)
        task = asyncio.create_task(agent.run('inspect'))
        await p.started.wait()
        row, = rows(_spend_ledger_in_tmp)
        assert (row['p'], row['m'], row['usd'], row['note']) == (
            'chatgpt', 'codex', None, 'ChatGPT plan, not per-call (computer task)')
        assert 'tok' not in row
        p.finish()
        await task
        assert len(rows(_spend_ledger_in_tmp)) == 1
    asyncio.run(go())


def test_warm_process_keeps_its_launch_auth_identity(monkeypatch, tmp_path, _spend_ledger_in_tmp):
    async def go():
        p = MeterProcess(totals=[[usage(1000)]])
        env, launches = wire(monkeypatch, p, tmp_path)
        pool = WarmCodex(CodexComputerAgent)
        pool.kick()
        await asyncio.gather(*pool._jobs)
        assert rows(_spend_ledger_in_tmp) == []
        monkeypatch.setattr(codex_auth, 'launch_environment', lambda: ({'HOME': str(tmp_path)}, 'plan'))
        agent = pool.take(lambda e: None, ask)
        await agent.run('inspect')
        await pool.close()
        assert len(launches) == 1 and launches[0][1] == env
        row, = rows(_spend_ledger_in_tmp)
        assert row['p'] == 'openai' and row['usd'] == pytest.approx(.002)
    asyncio.run(go())


def test_chat_prices_deltas_and_ignores_old_or_unrelated_events(monkeypatch, tmp_path, _spend_ledger_in_tmp):
    async def go():
        p = MeterProcess(totals=[[usage(1000, 200, 100)], [usage(4000, 500, 300)]], hold=True)
        wire(monkeypatch, p, tmp_path)
        chat = CodexChat(ask_user=ask)
        await chat.start(tmp_path, emit)
        await chat.send('first')
        assert rows(_spend_ledger_in_tmp) == []
        p.finish()
        await chat._turn_job
        monkeypatch.setattr(codex_auth, 'launch_environment', lambda: ({'HOME': str(tmp_path)}, 'plan'))
        await chat.send('second')
        await chat.send('correction')
        p.token_event(usage(99000), turn_id='turn-1')
        p.token_event(usage(99000), thread_id='another-thread')
        p.event('turn/completed', turn={'id': 'turn-1', 'status': 'completed'})
        await asyncio.sleep(0)
        assert chat.running and not chat._done.done()
        p.token_event(usage(4000, 500, 300))
        p.finish()
        await chat._turn_job
        await chat.close()
        first, second = rows(_spend_ledger_in_tmp)
        assert first['usd'] == pytest.approx(.00264)
        assert second['tok'] == {'in': 3000, 'cached': 300, 'out': 200}
        assert second['usd'] == pytest.approx(.00746)
        assert second['note'] == 'API key, billed per token (chat turn)'
        assert sum(m.get('method') == 'turn/start' for m in p.requests) == 2
    asyncio.run(go())


@pytest.mark.parametrize('model,totals', [('unknown-model', [[usage(1000)]]),
                                       (None, [[usage(1000)]]), ('gpt-6-sol', [])])
def test_unknown_model_or_absent_usage_is_unpriced(monkeypatch, tmp_path, _spend_ledger_in_tmp, model, totals):
    async def go():
        p = MeterProcess(model=model, totals=totals)
        wire(monkeypatch, p, tmp_path)
        await CodexComputerAgent(on_event=lambda e: None, ask_user=ask).run('inspect')
    asyncio.run(go())
    row, = rows(_spend_ledger_in_tmp)
    assert row['p'] == 'openai' and row['usd'] is None and row['unpriced']


@pytest.mark.parametrize('bad', [{}, None, {'inputTokens': True, 'cachedInputTokens': 0, 'outputTokens': 0},
                                usage(-1), usage(5, 6), {'inputTokens': '5'}])
def test_malformed_usage_never_becomes_free_or_guessed_spend(monkeypatch, tmp_path, _spend_ledger_in_tmp, bad):
    async def go():
        p = MeterProcess(totals=[[bad]])
        wire(monkeypatch, p, tmp_path)
        await CodexComputerAgent(on_event=lambda e: None, ask_user=ask).run('inspect')
    asyncio.run(go())
    row, = rows(_spend_ledger_in_tmp)
    assert row['usd'] is None and row['unpriced'] and 'tok' not in row


@pytest.mark.parametrize('chat_mode', [False, True])
def test_cancellation_collects_interrupt_usage_and_records_once(monkeypatch, tmp_path, _spend_ledger_in_tmp, chat_mode):
    async def go():
        p = MeterProcess(hold=True, interrupt_total=usage(1000, 100, 20))
        wire(monkeypatch, p, tmp_path)
        if chat_mode:
            agent = CodexChat(ask_user=ask)
            await agent.start(tmp_path, emit)
            await agent.send('work')
            await agent.close()
            await agent.close()
        else:
            agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask)
            task = asyncio.create_task(agent.run('work'))
            await p.started.wait()
            while agent.turn_id is None:
                await asyncio.sleep(0)
            agent.cancel()
            await task
        row, = rows(_spend_ledger_in_tmp)
        assert row['usd'] == pytest.approx(.00202)
        assert row['tok'] == {'in': 1000, 'cached': 100, 'out': 20}
    asyncio.run(go())


@pytest.mark.parametrize('chat_mode', [False, True])
def test_interrupt_ack_without_completion_never_prices_partial_usage(monkeypatch, tmp_path, _spend_ledger_in_tmp,
                                                                    chat_mode):
    async def go():
        p = MeterProcess(totals=[[usage(100)]], hold=True, interrupt_completed=False)
        wire(monkeypatch, p, tmp_path)
        if chat_mode:
            agent = CodexChat(ask_user=ask)
            await agent.start(tmp_path, emit)
            await agent.send('work')
            await agent.close()
        else:
            agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask)
            task = asyncio.create_task(agent.run('work'))
            await p.started.wait()
            while agent.turn_id is None:
                await asyncio.sleep(0)
            agent.cancel()
            await task
        row, = rows(_spend_ledger_in_tmp)
        assert row['usd'] is None and row['unpriced']
        assert row['tok'] == {'in': 100} and 'incomplete usage' in row['note']
    asyncio.run(go())


@pytest.mark.parametrize('totals,expected', [
    ([[], [usage(200)], [usage(300)]], [None, None, .0002]),
    ([[usage(200)], [usage(100)], [usage(300)]], [.0004, None, .0004]),
])
def test_chat_missing_or_reset_counter_does_not_charge_previous_turns(monkeypatch, tmp_path, _spend_ledger_in_tmp,
                                                                     totals, expected):
    async def go():
        p = MeterProcess(totals=totals)
        wire(monkeypatch, p, tmp_path)
        chat = CodexChat(ask_user=ask)
        await chat.start(tmp_path, emit)
        for _ in totals:
            await chat.send('work')
            await chat._turn_job
        await chat.close()
        assert [r['usd'] for r in rows(_spend_ledger_in_tmp)] == expected
    asyncio.run(go())


@pytest.mark.parametrize('prior_usage,expected', [(False, .01), (True, None)])
def test_rerouted_models_are_priced_only_when_usage_is_unambiguous(monkeypatch, tmp_path, _spend_ledger_in_tmp,
                                                                 prior_usage, expected):
    async def go():
        p = MeterProcess(totals=[[usage(100)]] if prior_usage else [], hold=True)
        wire(monkeypatch, p, tmp_path)
        chat = CodexChat(ask_user=ask)
        await chat.start(tmp_path, emit)
        await chat.send('work')
        p.event('model/rerouted', turnId=p.turn_id, fromModel='gpt-6-sol', toModel='gpt-6-astra', reason='test')
        p.token_event(usage(1000))
        p.finish()
        await chat._turn_job
        await chat.close()
        row, = rows(_spend_ledger_in_tmp)
        assert row['usd'] == expected
    asyncio.run(go())


def test_failed_api_launch_counts_one_unpriced_task(monkeypatch, tmp_path, _spend_ledger_in_tmp):
    monkeypatch.setattr(codex_auth, 'launch_environment', lambda: ({'HOME': str(tmp_path)}, 'api'))
    asyncio.run(CodexComputerAgent(on_event=lambda e: None, ask_user=ask, binary='/nonexistent/codex').run('work'))
    row, = rows(_spend_ledger_in_tmp)
    assert row['p'] == 'openai' and row['usd'] is None


def test_reusing_computer_agent_starts_fresh_usage_baseline(monkeypatch, tmp_path, _spend_ledger_in_tmp):
    async def go():
        agent = CodexComputerAgent(on_event=lambda e: None, ask_user=ask)
        for inp in (1000, 200):
            p = MeterProcess(totals=[[usage(inp)]])
            wire(monkeypatch, p, tmp_path)
            assert 'Done.' in await agent.run('inspect')
        assert [r['usd'] for r in rows(_spend_ledger_in_tmp)] == [.002, .0004]
    asyncio.run(go())


def test_missing_api_home_falls_back_in_both_environment_and_spend(monkeypatch, tmp_path, _spend_ledger_in_tmp):
    async def go():
        p = MeterProcess(totals=[[usage(1000)]])
        monkeypatch.setattr(codex_auth, 'eve_home', lambda: tmp_path / 'missing')
        monkeypatch.setenv('EVE_CODEX_AUTH', 'api')
        monkeypatch.setenv('CODEX_HOME', str(tmp_path / 'personal'))

        async def spawn(*args, **kw):
            assert kw['env']['CODEX_HOME'] == str(tmp_path / 'personal')
            return p

        monkeypatch.setattr(asyncio, 'create_subprocess_exec', spawn)
        await CodexComputerAgent(on_event=lambda e: None, ask_user=ask).run('inspect')
        row, = rows(_spend_ledger_in_tmp)
        assert row['p'] == 'chatgpt' and row['usd'] is None
        assert row['note'] == 'ChatGPT plan, not per-call (computer task)'
    asyncio.run(go())
