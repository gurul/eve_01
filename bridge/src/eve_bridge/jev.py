"""TypeSafe's Jev behind decider.Decider's `predict(state, questions)` seam.

`decider.Decider` is written against the System One signature, and Jev speaks that
signature over HTTP: `{"model", "state", "questions"}` in, `{"answers": {qid: {...}}}`
out, each `choice` answer carrying `choice` + `probabilities` + `confidence`. That is
the envelope `Decider._parse_choice` already parses, so this module is a transport, not
a translation — `predict` hands the decoded body straight back.

The shape is taken from browser-use/jev-ultrafast (MIT, `jev_ultrafast/model.py`), which
posts to the same service: `state` may be a dict, `criteria` values may be dicts, and
`answers` is keyed by the question id the caller chose. Its `validate_choice` is the same
set of assertions `_parse_choice` makes, so an answer that passes there passes here.

TWO ROUTES, one envelope:
  "typesafe"   POST https://api.typesafe.ai/v1/systemone      Bearer TYPESAFE_API_KEY
  "openrouter" POST https://openrouter.ai/api/alpha/decisions Bearer OPENROUTER_API_KEY
  "ollama"     POST http://127.0.0.1:11434/v1/systemone       no key: an open decision model on this Mac (Ollama
               0.35+ serves Jev's API; tev1 from Together AI, owner 2026-10-13: "its open source so run on device")
The first is the one jev-ultrafast exercises and the default here. The second is alpha;
it answers in the same envelope and is the route this Mac runs (2026-09-21: the head
classifier, the request router, and tools/jev_step_eval.py's 155 four-question requests
with 26 options each, 234 ms p50). A different envelope would read as `error`, never a
wrong click.

WHAT THIS CHANGES ABOUT THE BUDGET: `Decider` trims the tail of the option list before
predict to fit a small decision head (its default is 256 tokens). Jev takes 32k. The
budgets below are set past any real AX menu, so `trim_options` never drops an option and
`overflow` never fires — the two numbers fast_lane.py gates on stay about the model, not
about the wrapper.

WHAT IT COSTS: Jev is a network call; TypeSafe publish 70-500 ms, this Mac measures 234 ms
p50, and jev-ultrafast's 7.07 s is one whole 11-action browser task (about 0.64 s an
action), not a step. Against fast_lane's own cost model (+3.5 s a right click, -4.55 s a
wrong one) half a second of latency is small and accuracy is the whole question, which is
why `timeout_s` is short and every failure escalates to the planner rather than waiting.

WHAT IT COSTS THE OTHER WAY: Jev is a third party, and the state carries the focused
window's title and visible text. The daemon only builds that state during a computer-use
task the user spoke, but it is a new egress and the README's "nothing leaves the Mac" no
longer covers this lane when it is on. It is off by default.
"""

from __future__ import annotations

import json
import logging
import math
import os
import threading
import time
import urllib.error
import urllib.request
from typing import Any, Callable, Mapping, Optional

from . import pricing, spend
from .decider import WARMUP_CONTEXT, WARMUP_OBJECTIVE, WARMUP_OPTIONS, Decider

ROUTES = {
    "typesafe": ("https://api.typesafe.ai/v1/systemone", "TYPESAFE_API_KEY", "jev-latest"),
    "openrouter": ("https://openrouter.ai/api/alpha/decisions", "OPENROUTER_API_KEY", "typesafe/jev-1.13"),
    "ollama": ("http://127.0.0.1:11434/v1/systemone", "", "tev1:0.8b"),
}
KEYLESS = frozenset({"ollama"})   # served on this Mac: nothing to send but the request
DEFAULT_ROUTE = "typesafe"
DEFAULT_TIMEOUT_S = 3.0        # a click-path budget, not jev-ultrafast's 25 s browser budget
MAX_LEN = 32000                # Jev 1.13 context, per OpenRouter's /models endpoint (2026-09-21)
HEAD_MAX_LEN = 30000           # past any real AX menu: trim_options must never drop an option
RETRY_STATUS = frozenset({429, 500, 502, 503, 529})
RETRIES = 2                    # jev-ultrafast retries twice with 0.5 s * 2**n; one click step affords less


class JevError(RuntimeError):
    """One line, no traceback: `Decider.choose` turns this into `Choice.error`."""


class Meter:
    """What Jev has been asked so far in this process: calls, input tokens (the only billed kind) and
    milliseconds, from every `predict` this module makes. The computer agent snapshots it at the start and
    the end of a run and writes the difference into the run log's bill (pricing.estimate_jev_cost)."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.calls = 0
        self.input_tokens = 0
        self.ms = 0.0
        self.errors = 0

    def add(self, payload: Any, ms: float, *, error: bool = False) -> None:
        usage = payload.get("usage") if isinstance(payload, dict) and isinstance(payload.get("usage"), dict) else {}
        tokens = usage.get("input_tokens")
        with self._lock:
            self.calls += 1
            self.ms += ms
            self.errors += 1 if error else 0
            if isinstance(tokens, int) and not isinstance(tokens, bool):
                self.input_tokens += max(0, tokens)

    def snapshot(self) -> dict[str, Any]:
        with self._lock:
            return {"calls": self.calls, "input_tokens": self.input_tokens, "ms": round(self.ms), "errors": self.errors}

    @staticmethod
    def delta(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
        return {k: after[k] - before[k] for k in ("calls", "input_tokens", "ms", "errors")}


METER = Meter()


def meter_spend(url: str, model: str, payload: dict[str, Any]) -> None:
    """One answered decision into the daily spend meter (spend.py): OpenRouter's own usage.cost when its reply
    carries one, else the input tokens at Jev's rate (pricing.JEV_INPUT_PER_M; output is free). The provider is
    the route's host, so the owner sees which bill it lands on."""
    usage = payload.get("usage") if isinstance(payload.get("usage"), dict) else {}
    provider = "openrouter" if "openrouter.ai" in url else "typesafe"
    tokens = usage.get("input_tokens")
    tokens = tokens if isinstance(tokens, int) and not isinstance(tokens, bool) else 0
    if isinstance(usage.get("cost"), (int, float)) and not isinstance(usage.get("cost"), bool):
        spend.record(provider, model, spend.JEV, float(usage["cost"]), tokens={"in": tokens}, source="reported")
    elif usage:
        spend.record(provider, model, spend.JEV, pricing.estimate_jev_cost(tokens), tokens={"in": tokens})
    else:
        spend.record(provider, model, spend.JEV, None, note="no usage in the reply")


def timeout_from_env(env: Mapping[str, str]) -> float:
    """EVE_JEV_TIMEOUT in seconds, or DEFAULT_TIMEOUT_S when it is unset, blank, not a number, or not a
    positive finite one. One reader for every caller: a typo here used to raise out of the asker's builder
    (desktop_worker, browser_lane) or quietly switch the command-risk gate off (daemon)."""
    raw = str(env.get("EVE_JEV_TIMEOUT") or "").strip()
    if not raw:
        return DEFAULT_TIMEOUT_S
    try:
        seconds = float(raw)
    except ValueError:
        seconds = float("nan")
    if not math.isfinite(seconds) or seconds <= 0:
        logging.getLogger(__name__).warning("jev: EVE_JEV_TIMEOUT=%r is not a positive number of seconds; "
                                            "using %.1f", raw[:20], DEFAULT_TIMEOUT_S)
        return DEFAULT_TIMEOUT_S
    return seconds


def route_config(env: Mapping[str, str], route: str = "") -> tuple[str, str, str]:
    """(url, key, model) for `route`. Raises JevError when the route's key is unset."""
    name = (route or env.get("EVE_JEV_ROUTE") or DEFAULT_ROUTE).strip().lower()
    if name not in ROUTES:
        raise JevError(f"EVE_JEV_ROUTE must be one of {tuple(ROUTES)}, not {name!r}")
    url, key_name, default_model = ROUTES[name]
    key = (env.get(key_name) or "").strip() if key_name else ""
    if not key and name not in KEYLESS:
        raise JevError(f"{key_name} is not set (route {name})")
    model = (env.get("EVE_JEV_MODEL") or "").strip() or default_model
    return url, key, model


def make_predict(
    url: str,
    key: str,
    model: str,
    *,
    timeout_s: float = DEFAULT_TIMEOUT_S,
    opener: Optional[Callable[..., Any]] = None,
    sleep: Callable[[float], None] = time.sleep,
) -> Callable[[Any, dict[str, Any]], dict[str, Any]]:
    """A `predict(state, questions)` that posts one decision request and returns the body.

    `opener` takes urllib's `(request, timeout=...)` so the tests run without a socket.
    Raises JevError on anything that is not a decoded JSON object; `Decider` catches it.
    """
    send = opener or urllib.request.urlopen

    def predict(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        body = json.dumps({"model": model, "state": state, "questions": questions}).encode("utf-8")
        t0 = time.perf_counter()
        for attempt in range(RETRIES + 1):
            request = urllib.request.Request(
                url, data=body, method="POST",
                headers={**({"Authorization": f"Bearer {key}"} if key else {}), "Content-Type": "application/json"},
            )
            try:
                with send(request, timeout=timeout_s) as response:
                    payload = json.loads(response.read().decode("utf-8"))
            except urllib.error.HTTPError as e:
                if e.code in RETRY_STATUS and attempt < RETRIES:
                    sleep(0.2 * 2**attempt)
                    continue
                METER.add({}, (time.perf_counter() - t0) * 1000.0, error=True)
                raise JevError(f"HTTP {e.code} from {url}") from None
            except urllib.error.URLError as e:
                METER.add({}, (time.perf_counter() - t0) * 1000.0, error=True)
                raise JevError(f"{url} unreachable: {e.reason}") from None
            except (ValueError, TypeError) as e:
                METER.add({}, (time.perf_counter() - t0) * 1000.0, error=True)
                raise JevError(f"undecodable response from {url}: {type(e).__name__}") from None
            if not isinstance(payload, dict):
                METER.add({}, (time.perf_counter() - t0) * 1000.0, error=True)
                raise JevError(f"{url} returned {type(payload).__name__}, not an object")
            METER.add(payload, (time.perf_counter() - t0) * 1000.0)
            meter_spend(url, model, payload)
            return normalize(payload, questions)
        METER.add({}, (time.perf_counter() - t0) * 1000.0, error=True)
        raise JevError(f"{url} kept returning a retryable status")

    return predict


def normalize(payload: dict[str, Any], questions: Mapping[str, Any]) -> dict[str, Any]:
    """The body as `Decider` reads it: `{"answers": {qid: answer}, "usage": {...}}`.

    TypeSafe's own service already answers in that envelope (jev_ultrafast/model.py reads
    `result["answers"][qid]`), and so does OpenRouter's /api/alpha/decisions (the route this
    Mac runs), so the common path returns `payload` untouched. The one shape worth accepting
    beside it is the answers keyed at the top level. Anything else is passed through unchanged
    and fails in `_parse_choice` as "predict returned no answer", which the lane reads as
    unavailable — a wrong envelope must never become a click.
    """
    if isinstance(payload.get("answers"), dict):
        return payload
    keys = set(questions)
    if keys and keys <= set(payload) and all(isinstance(payload[k], dict) for k in keys):
        rest = {k: v for k, v in payload.items() if k not in keys}
        return {**rest, "answers": {k: payload[k] for k in keys}}
    return payload


def load(
    *,
    env: Optional[Mapping[str, str]] = None,
    style: str = "jev",
    route: str = "",
    timeout_s: float = 0.0,
    opener: Optional[Callable[..., Any]] = None,
) -> Decider:
    """A `Decider` backed by Jev, warmed up once so `available` means a real answer came back.

    The warm-up menu is decider.WARMUP_OPTIONS; any failure is a RuntimeError with one line,
    which `desktop_worker.start_fast_lane` turns into the lane's "failed: …" status.
    """
    t0 = time.perf_counter()
    source = env if env is not None else os.environ
    seconds = timeout_s or timeout_from_env(source)
    try:
        url, key, model = route_config(source, route)
    except JevError as e:
        raise RuntimeError(str(e)) from None
    decider = Decider(
        make_predict(url, key, model, timeout_s=seconds, opener=opener),
        max_len=MAX_LEN, head_max_len=HEAD_MAX_LEN, style=style,
    )
    decider.load_ms = (time.perf_counter() - t0) * 1000.0
    t1 = time.perf_counter()
    warm = decider.choose(WARMUP_OBJECTIVE, app="Calendar", context=WARMUP_CONTEXT, options=WARMUP_OPTIONS)
    decider.warm_ms = (time.perf_counter() - t1) * 1000.0
    if warm.error:
        raise RuntimeError(f"jev warm-up predict failed: {warm.error}")
    decider.available = True
    return decider
