"""Offline normal-browser America flow: semantic controls, one send, exact completion and Jev recovery."""

from __future__ import annotations

import asyncio
import copy
import json
import sys
import threading
from types import SimpleNamespace
from unittest.mock import Mock

import pytest
from test_america import ANSWER, ARGS, HEADERS, SOURCE, sse

from eve_bridge import america
from eve_bridge import america_browser as ab
from eve_bridge.typed_ask import StepAnswer

TEXT = f"Jurisdiction: {ARGS['jurisdiction']}\nQuestion: {ARGS['question']}"


class Locator:
    def __init__(self, page, kind):
        self.page, self.kind = page, kind

    def locator(self, css):
        return self.page.locator(css)

    def get_by_role(self, role, *, name, exact):
        if role == "textbox" and name == self.page.field_label:
            return Locator(self.page, "field")
        if role == "button" and name == self.page.send_label:
            return Locator(self.page, "send")
        return Locator(self.page, "missing")

    def count(self):
        return 0 if self.kind == "missing" else self.page.counts.get(self.kind, 1)

    def is_visible(self):
        return self.page.visible.get(self.kind, True)

    def is_enabled(self):
        return self.page.enabled.get(self.kind, True)

    def wait_for(self, **kw):
        if not self.is_visible():
            raise TimeoutError("private page body")

    def evaluate(self, code, arg=None):
        if code == ab._NAME_JS:
            return self.page.field_label if self.kind == "field" else self.page.send_label
        if "closest" in code:
            return self.kind in ("field", "send")
        return "textarea" if self.kind == "field" else "button"

    def get_attribute(self, name):
        return self.page.send_type if name == "type" else None

    def fill(self, text, **kw):
        assert self.kind == "field"
        self.page.value = text
        self.page.fills.append(text)

    def input_value(self, **kw):
        return self.page.value if self.page.value_correct else "private mismatch"

    def click(self, **kw):
        assert self.kind == "send"
        self.page.clicks += 1
        request = SimpleNamespace(method="POST", url=america.API_URL, post_data_json=copy.deepcopy(self.page.body))
        if request.post_data_json is None:
            request.post_data_json = {"language": "en", "id": f"freshchatid{len(self.page.gotos)}", "trigger": "submit-message",
                                      "messages": [{"role": "user", "parts": [{"type": "text", "text": self.page.value}], "id": "message"}]}
        request.response = lambda: self.page.response
        self.page.current = request
        self.page.emit("request", request)
        route = SimpleNamespace(request=request, continue_=lambda: self.page.sent.append(request),
                                abort=lambda: self.page.aborted.append(request))
        self.page.guard(route)
        if self.page.aborted:
            self.page.emit("requestfailed", request)
            return
        if self.page.duplicate:
            self.page.emit("request", request)
            self.page.guard(route)
        if self.page.click_error:
            raise RuntimeError("private browser error after submission")
        if self.page.failure:
            self.page.emit("requestfailed", request)
        elif not self.page.delayed:
            self.page.emit("requestfinished", request)


class Page:
    def __init__(self):
        self.url = "about:blank"
        self.response = SimpleNamespace(status=200, headers=HEADERS, body=lambda: sse())
        self.fills, self.gotos, self.listeners = [], [], {}
        self.value, self.clicks, self.closed, self.waits = "old private question", 0, False, 0
        self.counts, self.visible, self.enabled = {}, {}, {}
        self.field_label, self.send_label, self.send_type = "Describe what you need", "Send message", "submit"
        self.css_field = self.value_correct = True
        self.body = None
        self.click_error = self.failure = self.delayed = self.duplicate = False
        self.current = None
        self.release = None
        self.destination = "https://america.gov/"
        self.sent, self.aborted = [], []
        self.guard = None
        self.candidate_label = "private current-value fallback"
        self.page_title = "America.gov"

    def title(self):
        return self.page_title

    def locator(self, css):
        if css == ab.FORM:
            return Locator(self, "form")
        if css == ab.FIELD:
            return Locator(self, "field" if self.css_field else "missing")
        return Locator(self, "missing")

    def goto(self, url, **kw):
        self.gotos.append(url)
        self.url = url if url == "about:blank" else self.destination
        self.value = ""

    def on(self, event, callback):
        self.listeners.setdefault(event, []).append(callback)

    def remove_listener(self, event, callback):
        self.listeners[event].remove(callback)

    def route(self, url, callback):
        assert url == america.API_URL
        self.guard = callback

    def unroute(self, url, callback):
        assert url == america.API_URL and callback is self.guard
        self.guard = None

    def emit(self, event, request):
        for cb in self.listeners.get(event, []):
            cb(request)

    def wait_for_timeout(self, ms):
        self.waits += 1
        if self.release is not None:
            self.release.wait(0.01)
        if self.delayed and (self.release is None or self.release.is_set()):
            self.emit("requestfinished", self.current)

    def close(self):
        self.closed = True


class Lane:
    def __init__(self, page):
        self.page = page
        self.config = SimpleNamespace(attach=True)
        self._page = SimpleNamespace(page=page, snapshot=self.snapshot, _locator=self.locator)
        self.closed, self.snapshots = False, 0
        self.connects = 0

    def _ensure(self):
        self.connects += 1
        assert self._page is not None
        return self._page

    def snapshot(self):
        self.snapshots += 1
        return SimpleNamespace(elements=[
            SimpleNamespace(role="text field", label=self.page.candidate_label, value="private value", editable=True, id="field"),
            SimpleNamespace(role="button", label=self.page.send_label, value="", editable=False, id="send"),
            SimpleNamespace(role="button", label="private page unrelated button", editable=False, id="outside"),
        ])

    def locator(self, c):
        return Locator(self.page, c.id)

    async def _run(self, fn, *args):
        return fn(*args)

    async def close(self):
        self.closed = True
        self.page.close()


def flow(page=None, asker=None, **kw):
    page = page or Page()
    lane = Lane(page)
    return ab.AmericaBrowser(lane, step_asker=asker, **kw), page, lane


def ask(service):
    return asyncio.run(service.ask(ARGS["question"], ARGS["jurisdiction"]))


def test_stable_flow_fills_in_one_operation_sends_once_and_consumes_exact_sse():
    service, page, lane = flow(asker=Mock(side_effect=AssertionError("Jev must not run on known controls")))
    assert ask(service) == (ANSWER, [SOURCE])
    assert page.gotos == ["about:blank", america.AMERICA_URL]
    assert page.fills == [TEXT] and page.clicks == 1
    assert len(page.sent) == 1 and not page.aborted and page.guard is None
    assert lane.snapshots == 0 and not page.closed and all(not callbacks for callbacks in page.listeners.values())
    assert service.last_metrics["jev_calls"] == 0 and service.last_metrics["submitted"]


def test_repeated_questions_reuse_lane_but_unmount_prior_history_every_time():
    service, page, lane = flow()

    async def run():
        assert await service.ask(ARGS["question"], ARGS["jurisdiction"]) == (ANSWER, [SOURCE])
        page.value = "private previous question"
        assert await service.ask("Second question?", "California") == (ANSWER, [SOURCE])
        await service.close()
        await service.close()

    asyncio.run(run())
    assert page.gotos == ["about:blank", america.AMERICA_URL] * 2
    assert page.fills == [TEXT, "Jurisdiction: California\nQuestion: Second question?"]
    assert page.clicks == 2 and lane.closed


def test_jev_recovers_changed_names_then_reuses_semantic_recipe_without_private_values():
    page = Page()
    page.css_field, page.field_label, page.send_label = False, "Your government question", "Ask America"
    calls = []

    def choose(step, **kw):
        calls.append((step, kw))
        return StepAnswer("0", 0.98, 0.8, 0.99, 0, 0)

    service, _, lane = flow(page, choose)
    assert ask(service) == (ANSWER, [SOURCE])
    assert len(calls) == 2 and lane.snapshots == 2
    assert service._field.role == "textbox" and service._field.name == page.field_label
    assert service._send.name == page.send_label
    wire = json.dumps(calls)
    assert "private" not in wire and ARGS["question"] not in wire and "text field" in wire
    assert ask(service) == (ANSWER, [SOURCE]) and len(calls) == 2
    assert service.last_metrics["jev_calls"] == 0


def test_authorized_submit_recovery_keeps_confidence_gates_even_when_send_is_risky():
    page = Page()
    page.send_label = "Ask America"
    service, _, _ = flow(page, lambda *a, **kw: StepAnswer("0", 0.98, 0.8, 0.99, 0, 1))
    assert ask(service) == (ANSWER, [SOURCE]) and len(page.sent) == 1
    weak_page = Page()
    weak_page.send_label = "Ask America"
    weak, _, _ = flow(weak_page, lambda *a, **kw: StepAnswer("0", 0.4, 0.1, 0.4, 0, 1))
    with pytest.raises(ab.BrowserFailure):
        ask(weak)
    assert weak_page.clicks == 0 and not weak_page.sent


@pytest.mark.parametrize("answer", [StepAnswer("none", 1, 1, 0, 0, 0), StepAnswer("0", 0.4, 0.1, 0.4, 0, 0),
                                   StepAnswer("0", 0.99, 0.8, 0.99, 0, 1), StepAnswer("99", 1, 1, 1, 0, 0)])
def test_unavailable_or_uncertain_jev_cannot_send(answer):
    page = Page()
    page.css_field = False
    service, _, _ = flow(page, lambda *a, **kw: answer)
    with pytest.raises(ab.BrowserFailure):
        ask(service)
    assert page.clicks == 0 and page.closed


@pytest.mark.parametrize("change", [
    lambda p: p.counts.update(form=2), lambda p: p.visible.update(form=False),
    lambda p: p.enabled.update(field=False), lambda p: setattr(p, "send_type", "button"),
    lambda p: setattr(p, "value_correct", False), lambda p: p.enabled.update(send=False),
    lambda p: setattr(p, "destination", "https://attacker.test/chat"),
])
def test_invalid_or_ambiguous_controls_or_navigation_never_submit(change):
    service, page, _ = flow()
    change(page)
    with pytest.raises(ab.BrowserFailure):
        ask(service)
    assert page.clicks == 0 and page.closed


def test_post_click_error_does_not_retry_and_removes_listeners():
    service, page, _ = flow()
    page.click_error = True
    with pytest.raises(ab.BrowserFailure, match="may have been sent") as exc:
        ask(service)
    assert "private" not in str(exc.value) and page.clicks == 1 and page.closed
    assert all(not callbacks for callbacks in page.listeners.values())


def test_observed_browser_security_page_has_no_question_or_jev_call():
    service, page, lane = flow(asker=Mock(side_effect=AssertionError("do not ground a challenge")))
    page.page_title = "Just a moment..."
    with pytest.raises(ab.BrowserFailure, match="security check"):
        ask(service)
    assert not page.fills and page.clicks == 0 and not page.sent and lane.snapshots == 0


@pytest.mark.parametrize("change", [
    lambda p: setattr(p, "failure", True), lambda p: setattr(p, "duplicate", True),
    lambda p: setattr(p.response, "status", 418), lambda p: setattr(p.response, "headers", {"content-type": "text/html"}),
    lambda p: setattr(p.response, "body", lambda: sse()[:-16]),
    lambda p: setattr(p.response, "body", lambda: b"x" * (america.MAX_STREAM_BYTES + 1)),
    lambda p: setattr(p, "body", {"messages": [{"role": "user", "parts": [{"type": "text", "text": "old private question"}]}]}),
])
def test_failed_unsourced_or_uncorrelated_request_has_no_answer_and_no_second_click(change):
    service, page, _ = flow()
    change(page)
    with pytest.raises(ab.BrowserFailure) as exc:
        ask(service)
    assert "private" not in str(exc.value) and page.clicks == 1 and page.closed


def test_delayed_completion_waits_for_finished_response():
    service, page, _ = flow()
    page.delayed = True
    assert ask(service) == (ANSWER, [SOURCE]) and page.waits == 1


@pytest.mark.parametrize("extra", ["history", "metadata", "top_metadata", "reused_id"])
def test_prior_history_or_signed_metadata_is_rejected_before_network_egress(extra):
    service, page, _ = flow()
    page.body = {"language": "en", "id": "seeded", "trigger": "submit-message", "messages": [
        {"id": "freshmessage", "role": "user", "parts": [{"type": "text", "text": TEXT}]}]}
    if extra == "history":
        page.body["messages"].append({"role": "assistant", "parts": []})
    elif extra == "metadata":
        page.body["messages"][0]["metadata"] = {"signed": "private earlier turn"}
    elif extra == "top_metadata":
        page.body["metadata"] = {"signed": "private earlier turn"}
    else:
        service._recent_ids.append("seeded")
    with pytest.raises(ab.BrowserFailure, match="fresh question"):
        ask(service)
    assert page.clicks == 1 and len(page.aborted) == 1 and page.sent == []


def test_cancelled_worker_is_quarantined_and_new_question_is_not_queued():
    service, page, lane = flow(timeout_secs=1)
    release, blocked = threading.Event(), threading.Event()
    page.delayed = True

    def block(ms):
        blocked.set()
        assert release.wait(3), "test did not release controlled browser worker"

    page.wait_for_timeout = block

    async def worker(fn, *args):
        return await asyncio.to_thread(fn, *args)

    lane._run = worker

    async def run():
        pending = asyncio.create_task(service.ask(ARGS["question"], ARGS["jurisdiction"]))
        while not blocked.is_set():
            await asyncio.sleep(0.001)
        try:
            pending.cancel()
            with pytest.raises(asyncio.CancelledError):
                await pending
            assert not service._inflight.done()
            with pytest.raises(ab.BrowserFailure, match="previous"):
                await service.ask("Do not send me", "California")
        finally:
            release.set()
        await asyncio.gather(service._inflight, return_exceptions=True)
        await service.close()

    asyncio.run(run())
    assert page.clicks == 1 and page.closed and lane.closed


def test_total_worker_budget_expires_and_closes_the_owned_page():
    ticks = iter([0, 0, 0, 0, 0, 0, 0, 0, 0, 2])
    service, page, _ = flow(timeout_secs=1, clock=lambda: next(ticks, 2))
    page.delayed, page.release = True, threading.Event()
    with pytest.raises(ab.BrowserFailure, match="timed out"):
        ask(service)
    assert page.closed and page.clicks <= 1


def test_factory_requires_normal_chrome_and_respects_america_jev_opt_out(monkeypatch):
    assert ab.make_browser({}) is None
    calls = []
    monkeypatch.setitem(sys.modules, "playwright", SimpleNamespace())
    monkeypatch.setattr(ab.browser_lane, "make_step_asker", lambda env: calls.append(env) or None)
    service = ab.make_browser({"EVE_BROWSER_OWN": "1", "EVE_AMERICA_JEV": "0"})
    assert service.lane.config.attach and service.lane.config.own
    assert calls[0]["EVE_JEV_STEP"] == "0"
    asyncio.run(service.close())


def test_tool_uses_browser_by_default_and_keeps_normalized_answer_contract():
    service, page, _ = flow()
    provider = america.America(america.AmericaConfig(True, transport="browser"), browser=service)
    result = asyncio.run(provider.handle("america_ask", ARGS))
    assert result["ok"] and result["provider"] == "america_gov" and result["answer"] == ANSWER and result["sources"] == [SOURCE]
    assert page.clicks == 1
    asyncio.run(provider.close())


def test_tool_browser_failure_does_not_fall_back_to_a_direct_post(monkeypatch):
    blocked = Mock(side_effect=AssertionError("direct HTTP attempted"))
    with pytest.raises(AssertionError, match="direct HTTP attempted"):
        blocked()  # positive control: the absence sentinel detects a direct call
    blocked.reset_mock()
    monkeypatch.setattr("httpx.AsyncClient", blocked)
    service, page, _ = flow()
    page.counts["form"] = 0
    provider = america.America(america.AmericaConfig(True, transport="browser"), browser=service)
    result = asyncio.run(provider.handle("america_ask", ARGS))
    assert not result["ok"] and result["status"] == "upstream_unavailable" and "answer" not in result
    assert page.clicks == 0
    blocked.assert_not_called()


def test_transport_configuration_is_explicit_and_invalid_modes_rejected():
    assert america.configured({}).transport == "alexandria"
    assert america.configured({"EVE_AMERICA_TRANSPORT": "api"}).transport == "api"
    with pytest.raises(ValueError, match="alexandria, browser or api"):
        america.configured({"EVE_AMERICA_TRANSPORT": "stealth"})
