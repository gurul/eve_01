"""Fresh, folder-scoped Telegram chats through the public Codex app-server.

This session owns its new thread. It never selects, resumes, or writes to an
existing desktop task. Saved folder metadata is read-only; transcripts stay in
Codex's own thread storage, not Eve's diagnostic logs.
"""
from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

from .codex_computer import (
    BROWSER_INSTRUCTIONS,
    ONCE_ANSWERS,
    OWNER_ACTION_INSTRUCTIONS,
    CodexComputerAgent,
    CodexUnavailable,
    meter_codex,
)

log = logging.getLogger(__name__)

CHAT_INSTRUCTIONS = """The owner is chatting with you through Eve on Telegram.
This is a new conversation in the selected folder. Follow its AGENTS.md and the
owner's requests. Keep replies and progress concise. Use your normal coding tools
for repository work and the installed cua API for UI work. Ask consequential
questions through request_user_input or native permission requests; Eve relays
them to the owner. Your public answers are delivered automatically; do not also
send duplicate replies through Telegram tools. Task-requested Telegram sends are
authorized. Do not claim an action succeeded without evidence.
ChatGPT's native UI is blocked by Computer Use; do not bypass that restriction.
""" + OWNER_ACTION_INSTRUCTIONS + BROWSER_INSTRUCTIONS


def accessible_folders(home: Path | None = None) -> list[Path]:
    """Only existing, accessible local roots saved in Codex, never task history."""
    home = home or Path(os.environ.get('CODEX_HOME', Path.home() / '.codex'))
    try:
        state = json.loads((home / '.codex-global-state.json').read_text())
        projects = state.get('local-projects', {})
        if not isinstance(projects, dict):
            raise ValueError('Invalid folder catalog')
        roots = set()
        for project in projects.values():
            if not isinstance(project, dict):
                continue
            paths = project.get('rootPaths', [])
            if not isinstance(paths, list):
                continue
            for root in paths:
                if not isinstance(root, str) or not Path(root).is_absolute():
                    continue
                path = Path(root).resolve()
                if path.is_dir() and os.access(path, os.R_OK | os.X_OK):
                    roots.add(path)
        return sorted(roots, key=lambda p: (p.name.casefold(), str(p)))
    except (OSError, ValueError, TypeError) as exc:
        raise CodexUnavailable('Could not read saved Codex folders.') from exc


def folder_menu(folders: list[Path]) -> str:
    folders = [p for p in folders if not any(part.casefold().lstrip('.') == 'debrief' for part in p.parts)]
    return '\n'.join(str(p) if sum(q.name.casefold() == p.name.casefold() for q in folders) > 1
                     else p.name for p in folders)


def select_folder(folders: list[Path], selection: str) -> Path:
    value = selection.strip().strip('"\'').rstrip('/') or '/'
    expanded = str(Path(value).expanduser()).casefold()
    matches = [p for p in folders if p.name.casefold() == value.casefold() or str(p).casefold() == expanded]
    if len(matches) == 1:
        return matches[0]
    if matches:
        raise CodexUnavailable('More than one folder has that name. Use its full path:\n' +
                               '\n'.join(str(p) for p in matches))
    raise CodexUnavailable('That folder is not available. Send codex on to see folders.')


class CodexChat(CodexComputerAgent):
    def __init__(self, *, ask_user: Callable[[str], Awaitable[str]], **kw):
        super().__init__(on_event=self._event, ask_user=ask_user, **kw)
        self._output: Callable[[str], Awaitable[None]] | None = None
        self._picture: Callable[[], Awaitable[None]] | None = None
        # The turn's result, when the caller tells it apart from progress (Telegram edits one progress
        # message and sends the result as a new one); without it the result goes to the output like a step.
        self._result: Callable[[str], Awaitable[None]] | None = None
        self._turn_job: asyncio.Task | None = None
        self._outputs: set[asyncio.Task] = set()     # steps on their way to the caller, awaited before the result
        self._connected = False
        self.cwd: Path | None = None

    @property
    def connected(self) -> bool:
        return (self._connected and self._proc is not None and self._proc.returncode is None
                and self._reader is not None and not self._reader.done())

    def _event(self, event) -> None:
        if event.kind == 'progress' and self._output:
            # Not one of the permission jobs _finish_turn cancels: a step is delivered, never dropped.
            task = asyncio.create_task(self._output(event.text))
            self._outputs.add(task)
            task.add_done_callback(self._outputs.discard)
            task.add_done_callback(lambda t: None if t.cancelled() else t.exception())

    async def start(self, folder: Path, emit: Callable[[str], Awaitable[None]],
                    picture: Callable[[], Awaitable[None]] | None = None,
                    done: Callable[[str], Awaitable[None]] | None = None) -> None:
        await self.close()
        self._cancelled = False
        self.cwd, self._output, self._picture, self._result = folder, emit, picture, done
        if not folder.is_dir() or not os.access(folder, os.R_OK | os.X_OK):
            raise CodexUnavailable('That folder is no longer accessible.')
        try:
            await self._launch()
            result = await self._rpc('thread/start', {
                'cwd': str(folder), 'ephemeral': False, 'sandbox': 'workspace-write',
                'approvalPolicy': 'on-request', 'developerInstructions': CHAT_INSTRUCTIONS,
            })
            self._thread_started(result)
            if Path(result['thread']['cwd']).resolve() != folder.resolve():
                raise CodexUnavailable('Codex opened a different folder. The chat was closed.')
            self._connected = True
            log.info('codex chat: started thread=%s', self.thread_id)
        except (OSError, TimeoutError, KeyError, CodexUnavailable) as exc:
            await self.close()
            raise CodexUnavailable('Could not start a fresh Codex chat. Eve is still available.') from exc

    async def send(self, text: str) -> None:
        if not self.connected:
            raise CodexUnavailable('Codex disconnected. Send codex <folder> to start a new chat.')
        if self._turn_job is not None and self._done is not None and self._done.done():
            await self._turn_job
        if self.running:
            if not self.turn_id:
                raise CodexUnavailable('Codex is still starting your last message. Please wait.')
            await self._rpc('turn/steer', {'threadId': self.thread_id, 'expectedTurnId': self.turn_id,
                                         'input': [{'type': 'text', 'text': text}]})
            return
        self.running = True
        self.goal, self.final, self.last_commentary = text, '', ''
        self.turn_id = None
        self.browser_used, self.browser_screenshot, self.browser_tab_id = False, None, None
        self.ui_evidence = []
        self._done = asyncio.get_running_loop().create_future()
        self._begin_metering('chat turn')
        try:
            result = await self._rpc('turn/start', {'threadId': self.thread_id,
                                                   'input': [{'type': 'text', 'text': text}]})
            self.turn_id = result['turn']['id']
            self._meter_turn_id = self.turn_id
        except (OSError, TimeoutError, KeyError, CodexUnavailable) as exc:
            await self.close()
            raise CodexUnavailable('Codex did not confirm the message. It was not retried. '
                                   'The chat is closed; check Codex before sending the work again.') from exc
        if self._auth_mode == 'plan':
            meter_codex('chat turn')
        self._turn_job = asyncio.create_task(self._finish_turn())

    async def _finish_turn(self) -> None:
        try:
            turn = await asyncio.wait_for(asyncio.shield(self._done), self.max_secs)
            # A step emitted just before the turn completed is delivered first, in order: cancelled with the
            # jobs below it was lost, and left running it arrived after the result (review, 2026-09-23).
            outputs = [t for t in self._outputs if not t.done()]
            if outputs:
                await asyncio.gather(*outputs, return_exceptions=True)
            # An interrupted/failed turn can leave a permission question waiting.
            # Cancel it before the next turn so its answer cannot approve stale work.
            jobs = list(self._jobs)
            for job in jobs:
                job.cancel()
            await asyncio.gather(*jobs, return_exceptions=True)
            status = turn.get('status')
            result = (self.final or 'Codex ended without a reply.') if status == 'completed' else (
                'Codex stopped.' if status == 'interrupted' else 'Codex could not finish this message.')
            if self._result or self._output:
                await (self._result or self._output)(result)
            if status == 'completed' and self.browser_used and self._picture:
                await self._picture()
        except asyncio.CancelledError:
            raise
        except (TimeoutError, CodexUnavailable):
            self._connected = False
            with contextlib.suppress(CodexUnavailable, TimeoutError):
                await asyncio.wait_for(self.interrupt(), 3)
            if self._result or self._output:
                await (self._result or self._output)('Codex stopped responding. No work was retried. '
                                   'Send codex off to return to Eve, or codex <folder> for a new chat.')
        finally:
            if not self._cancelled:
                self._finish_metering()
            self.running = False
            self.turn_id = None

    async def interrupt(self) -> None:
        if not self.running or not self.turn_id:
            raise CodexUnavailable('Nothing is running in this Codex chat.')
        await self._rpc('turn/interrupt', {'threadId': self.thread_id, 'turnId': self.turn_id})

    async def _answer_request(self, msg: dict[str, Any]) -> None:
        method, params = msg['method'], msg.get('params', {})
        if method not in {'item/commandExecution/requestApproval', 'item/fileChange/requestApproval'}:
            await super()._answer_request(msg)
            return
        decision = 'decline'
        async with self._approval_lock:
            if params.get('threadId') == self.thread_id and not self._cancelled:
                if method == 'item/commandExecution/requestApproval':
                    detail = params.get('command')
                    if detail:
                        question = f'Codex asks to run this command in {params.get("cwd", self.cwd)}:\n{detail}'
                    else:
                        question = ''
                else:
                    # This protocol form has no patch content to review here.
                    question = ''
                if question:
                    reason = params.get('reason') or ''
                    try:
                        answer = await self.ask_user(question + '\n' + reason + '\nReply yes or no.')
                        if answer.strip().lower().rstrip('.!') in ONCE_ANSWERS:
                            decision = 'accept'
                    except Exception:  # noqa: BLE001 — a question that could not be asked is a decline, sent
                        pass               # now: unanswered, Codex would wait on the approval until max_secs
                else:
                    self._emit('progress', 'Codex requested permission without enough detail to review here. '
                               'It was declined; no permission was changed.')
            if not self._cancelled:
                await self._send({'id': msg['id'], 'result': {'decision': decision}})

    async def close(self) -> None:
        self._connected, self._cancelled = False, True
        if self.running and self.turn_id and self._done is not None and not self._done.done():
            with contextlib.suppress(Exception):
                await asyncio.wait_for(self.interrupt(), 3)
        if self._turn_job is not None:
            self._turn_job.cancel()
            await asyncio.gather(self._turn_job, return_exceptions=True)
            self._turn_job = None
        await self._close()
        self._finish_metering()
        self._proc = self._reader = self._done = None
        self.thread_id = self.turn_id = None
        self.running = False
        self._output = self._picture = self._result = None
