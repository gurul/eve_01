#!/usr/bin/env python3
"""Score the chief of staff's capture (design 5.2 E1): does the real Telegram brain file a card when it should.

    .venv/bin/python tools/chief_eval.py --capture                    # score the blind set ONCE (live luna, cents)
    .venv/bin/python tools/chief_eval.py --capture --check-default    # assert chief.SHIPPED equals the verdict (G7.3)

The blind set (tests/fixtures/chief/capture_holdout.json, 2026-09-29) was written by another agent that had not
seen the code, and its sha256 (capture_holdout.sha256) was committed before the scoring. Each case is one owner
message, labelled ``card`` (hand it to the chief: ``acceptable_done`` lists the done kinds that would prove it) or
``no_card`` (today's tools answer it).

**What is asked, exactly.** The Telegram brain as the daemon builds it (daemon.py ``_make_telegram``), with the
chief on: the owner's env file loaded the daemon's way (envfile.load_env_file over the launchd plist's bare
environment, PATH and HOME), telegram.configured() (luna, its effort, self_context's block), the real tools and
instructions (TelegramInlet._turn_parts: memory, the second brain, the watcher, Meet, the lights, Spotify, the
owner's apps (Composio and the direct apps_mcp ones), the app maker, the chief's take_on and jobs_list), and the turn's note (turn_context plus
the chief's line). Each case is the first message of a fresh chat, sent through TelegramInlet._think itself, so the
request is byte for byte the one a text would make. Three things differ, all on the side of doing nothing: the
chief's ledger and the spend rows go to a temp folder (the owner's real ones are never read or written), mem0 is not
opened (the live daemon holds its one opener; the memory tools are the same fixed list either way), and the
Composio session is resumed from a copy of its state file. **No executor runs**: TelegramInlet._tool is replaced by
a recorder that stops the turn at the first tool call, so the round-1 tool choice is the decision scored (design
5.1 R1: take_on or the other tools). A take_on's arguments are also run through chief_card.parse (pure code) to
report refusals; nothing is filed.

**PRE-REGISTERED BAR** (design 5.2 E1, copied here and next to chief.SHIPPED on 2026-09-29, before any scoring;
the scoring rules below were fixed at the same time). On the blind set (>= 60 cases, >= 20 card, >= 20 no_card),
scored exactly once, pass needs all of:

    (a) cards on no_card cases <= 10%                  (no_card cases whose round 1 called take_on / no_card cases)
    (b) of card cases that got a card, >= 90% have a done kind in acceptable_done
                                                       (at least one done_checks kind is in the case's list; no
                                                        card on any card case at all fails (b))
    (c) cards that ask a question <= 25%               (take_on calls whose question is a non-empty string / all
                                                        take_on calls, on any case)
    (d) one-way phases dispatched without a Go = 0     (``dispatched`` lines in the eval's events log; with no
                                                        executor it is 0 by construction, so the one-yes-one-act
                                                        tests, ``ONE_YES_TESTS``, are cited beside it)

Missed captures (card cases with no card) are reported, not gated: they fall back to today's tools. A case whose
request still fails after ``ATTEMPTS`` tries, or whose reply the turn cannot read, counts against every gate it
touches (fail-closed): on a no_card case it is a card; on a card case it is a card with no acceptable kind that asks
a question. A pass sets chief.SHIPPED = True; otherwise it stays False. Nothing is tuned after the scoring.

The verdict is kept in capture_result.json beside the set (case index, label, what the brain did: never the
owner's words, nothing from their profile), with the set's sha256. ``--check-default`` recomputes the bar from
those rows, deterministically and offline, and asserts chief.SHIPPED equals it; before a scoring it asserts
SHIPPED is False. A second ``--capture`` refuses: a blind set is scored once.
"""

from __future__ import annotations

import argparse
import asyncio
import dataclasses
import hashlib
import json
import os
import shutil
import sys
import tempfile
import time
from dataclasses import asdict, dataclass
from datetime import date, datetime
from pathlib import Path
from typing import Any, Mapping, Optional, Sequence

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge import chief  # noqa: E402

DATA_DIR = Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "chief"
HOLDOUT = "capture_holdout.json"
HOLDOUT_SHA = "capture_holdout.sha256"
RESULT = "capture_result.json"

# The pre-registered bar (design 5.2 E1, 2026-09-29, before any scoring). chief.SHIPPED carries the same text.
MIN_CASES = 60
MIN_EACH = 20
MAX_NO_CARD_CARDS = 0.10            # (a)
MIN_DONE_OK = 0.90                  # (b)
MAX_ASKING = 0.25                   # (c)
MAX_UNGATED_ONE_WAY = 0             # (d)
ATTEMPTS = 3                        # a request that fails to arrive is sent again; the model's answer never is
# (d)'s evidence beside the events log: the tests that hold one yes to one act (run with PYTEST, cited in the report).
ONE_YES_TESTS = ("tests/test_chief.py tests/test_telegram_chief.py -k \"go_timeout or never_a_yes or go_by_id or "
                 "stale_or_spent_yes or yes_to_an_old_go or approval_is_bound or bare_yes or verb_pattern_misses or "
                 "go_c12 or opens_with_ok or plain_yes_approves or restart_after_an_act_dispatch or "
                 "go_never_survives or go_is_asked\"")
# The launchd plist's whole environment (_service_launchd.py); the env file fills in the rest (envfile.py).
DAEMON_ENV = ("PATH", "HOME", "EVE_SERIAL_PORT")


@dataclass(frozen=True)
class Row:
    """What the brain did with one case. Never the case's words: the index names it."""
    i: int
    label: str                      # "card" | "no_card"
    took: bool                      # round 1 called take_on
    kinds: tuple[str, ...]          # its done_checks kinds
    question: bool                  # its question is a non-empty string
    tool: str                       # the first tool round 1 called ("" for a text reply)
    refused: str                    # chief_card.parse's refusal for the take_on, or ""
    error: str                      # the error type when the case could not be scored, or ""


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def committed_sha(data_dir: Path) -> str:
    return (data_dir / HOLDOUT_SHA).read_text(encoding="utf-8").split()[0].strip()


def capture_bar(rows: Sequence[Row], cases: Sequence[Mapping[str, Any]], dispatched: int) -> dict[str, Any]:
    """The pre-registered bar, exactly, as {"n", "card_cases", "no_card_cases", "set_ok", "no_card_cards",
    "a_rate", "a", "carded", "done_ok", "b_rate", "b", "cards", "asking", "c_rate", "c", "dispatched", "d",
    "missed", "errors", "shipped"}. An errored row counts against every gate it touches (the module's text)."""
    if len(rows) != len(cases):
        raise ValueError(f"{len(rows)} rows for {len(cases)} cases")
    card = [r for r in rows if r.label == "card"]
    no_card = [r for r in rows if r.label == "no_card"]
    set_ok = len(rows) >= MIN_CASES and len(card) >= MIN_EACH and len(no_card) >= MIN_EACH and \
        len(card) + len(no_card) == len(rows)
    no_card_cards = sum(1 for r in no_card if r.took or r.error)
    carded = [r for r in card if r.took or r.error]

    def done_ok(r: Row) -> bool:
        acceptable = {str(k) for k in (cases[r.i].get("acceptable_done") or [])}
        return not r.error and bool(set(r.kinds) & acceptable)

    good = sum(1 for r in carded if done_ok(r))
    cards = [r for r in rows if r.took or r.error]
    asking = sum(1 for r in cards if r.question or r.error)
    a_rate = no_card_cards / len(no_card) if no_card else 1.0
    b_rate = good / len(carded) if carded else 0.0
    c_rate = asking / len(cards) if cards else 0.0
    a, b = bool(no_card) and a_rate <= MAX_NO_CARD_CARDS, bool(carded) and b_rate >= MIN_DONE_OK
    c, d = c_rate <= MAX_ASKING, dispatched <= MAX_UNGATED_ONE_WAY
    return {"n": len(rows), "card_cases": len(card), "no_card_cases": len(no_card), "set_ok": set_ok,
            "no_card_cards": no_card_cards, "a_rate": a_rate, "a": a,
            "carded": len(carded), "done_ok": good, "b_rate": b_rate, "b": b,
            "cards": len(cards), "asking": asking, "c_rate": c_rate, "c": c,
            "dispatched": dispatched, "d": d,
            "missed": [r.i for r in card if not r.took and not r.error], "errors": [r.i for r in rows if r.error],
            "shipped": set_ok and a and b and c and d}


def _pct(x: float) -> str:
    return f"{100 * x:.1f}%"


def report(bar: Mapping[str, Any], rows: Sequence[Row], cases: Sequence[Mapping[str, Any]]) -> None:
    """The scored numbers, gate by gate, then every case a gate counted against (index, label, what the brain
    did) and the missed captures."""
    print(f"== E1 capture / blind {HOLDOUT}: n={bar['n']} (card {bar['card_cases']}, no_card "
          f"{bar['no_card_cases']}; need >= {MIN_CASES}, >= {MIN_EACH} each): {'ok' if bar['set_ok'] else 'FAIL'}")
    print(f"   (a) cards on no_card cases {bar['no_card_cards']} of {bar['no_card_cases']} = {_pct(bar['a_rate'])} "
          f"(need <= {_pct(MAX_NO_CARD_CARDS)}): {'pass' if bar['a'] else 'FAIL'}")
    print(f"   (b) card cases that got a card with an acceptable done kind {bar['done_ok']} of {bar['carded']} = "
          f"{_pct(bar['b_rate'])} (need >= {_pct(MIN_DONE_OK)}): {'pass' if bar['b'] else 'FAIL'}")
    print(f"   (c) cards that ask a question {bar['asking']} of {bar['cards']} = {_pct(bar['c_rate'])} "
          f"(need <= {_pct(MAX_ASKING)}): {'pass' if bar['c'] else 'FAIL'}")
    print(f"   (d) one-way phases dispatched without a Go {bar['dispatched']} (events log; need "
          f"{MAX_UNGATED_ONE_WAY}): {'pass' if bar['d'] else 'FAIL'}")
    print(f"   missed captures (reported, not gated): {len(bar['missed'])} of {bar['card_cases']}")
    for r in rows:
        acceptable = list(cases[r.i].get("acceptable_done") or [])
        why = []
        if r.error:
            why.append(f"ERROR {r.error}")
        elif r.label == "no_card" and r.took:
            why.append("card on a no_card case")
        elif r.label == "card" and r.took and not set(r.kinds) & set(acceptable):
            why.append(f"done kinds {list(r.kinds)} not in {acceptable}")
        if r.took and r.question and not r.error:
            why.append("asks a question")
        if r.refused:
            why.append(f"parse refused: {r.refused}")
        if why:
            print(f"   case {r.i:>2} [{r.label}] tool={r.tool or '-'}: {'; '.join(why)}")
    for i in bar["missed"]:
        print(f"   miss   case {i:>2} [card] tool={rows[i].tool or '(text reply)'}")


# ---- the live brain -----------------------------------------------------------------------------------------

class _Stopped(Exception):
    """The recorder stopped the turn at its first tool call: nothing runs."""


class _NoApi:
    """The Telegram bot API, lent to nothing: every call is a bug in the eval."""

    def __getattr__(self, name: str) -> Any:
        raise RuntimeError(f"the capture eval never talks to Telegram ({name})")


class _Tunnel:
    """The Mini App as ChatMaker reads it for its tools: a live URL (the daemon's tunnel is up)."""
    url = "https://miniapp.invalid"


def _daemon_environment(scratch: Path) -> None:
    """The daemon's environment: the plist's keys, then the owner's env file the daemon's way (the file fills gaps,
    never overrides), then the chief on with its ledger and the spend rows in ``scratch``. Values are never
    printed."""
    from eve_bridge.envfile import load_env_file

    keep = {k: os.environ[k] for k in DAEMON_ENV if k in os.environ}
    os.environ.clear()
    os.environ.update(keep)
    load_env_file()
    os.environ["EVE_CHIEF"] = "on"
    os.environ["EVE_CHIEF_DIR"] = str(scratch / "chief")
    os.environ["EVE_SPEND_DIR"] = str(scratch / "spend")


def _brain(scratch: Path) -> tuple[Any, Any, list[str]]:
    """(inlet, chief, notes): the Telegram inlet as daemon.py _make_telegram builds it, with the chief on, a
    recorder for ``_tool`` and a creator that tries a failed request again. ``notes`` says what was lent."""
    from eve_bridge import (
        apps_maker,
        apps_mcp,
        composio_tools,
        lights,
        meet,
        memory,
        miniapp,
        recall,
        second_brain,
        spotify,
        transcripts,
        watch,
    )
    from eve_bridge import telegram as tg
    from eve_bridge.computer_agent import make_response_creator
    from eve_bridge.notes import notes_dir

    cfg = tg.configured()
    if not cfg.enabled:
        raise SystemExit("NO_TELEGRAM: the owner's env does not turn the Telegram door on")
    notes = [f"model {cfg.model}, effort {cfg.effort}"]
    recall_cfg = recall.configured()
    mem = None
    if memory.enabled():
        mem = memory.Memory(recall_cfg, transcripts.Transcripts(transcripts.configured(),
                                                                archive=recall_cfg.archive_dir), None)
    notes.append(f"memory {'on' if mem is not None else 'off'}")
    vault = second_brain.configured()
    watcher = watch.make_watcher()
    apps = None
    app_cfg = composio_tools.configured(os.environ, cfg.owner_ids)
    if app_cfg.enabled:
        state = scratch / "composio-state.json"
        if Path(app_cfg.state_path).expanduser().exists():
            shutil.copyfile(Path(app_cfg.state_path).expanduser(), state)
        bridge = composio_tools.ComposioBridge(dataclasses.replace(app_cfg, state_path=state))
        try:
            bridge.start()
            apps = bridge
        except Exception as e:  # noqa: BLE001 — the daemon runs on without its apps too; said below
            notes.append(f"apps unavailable ({type(e).__name__})")
    direct = None
    direct_cfg = apps_mcp.configured(os.environ)
    if direct_cfg.enabled:
        direct = apps_mcp.AppsBridge(direct_cfg)
        direct.start()                                     # lists tools; an app that fails is left out
        notes.append(f"direct apps {', '.join(direct.apps) or 'none'}")
    lights_hub, spotify_hub = lights.make_lights(), spotify.make_spotify()
    meeter = meet.make_meeter(None, notes_dir(recall_cfg), apps=direct)
    real = make_response_creator()

    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        last: Optional[BaseException] = None
        for attempt in range(ATTEMPTS):
            try:
                return await real(payload)
            except Exception as e:  # noqa: BLE001 — the type only; tried again, then the case is an error
                last = e
                await asyncio.sleep(2.0 * (attempt + 1))
        assert last is not None
        raise last

    inlet = tg.TelegramInlet(
        cfg, _NoApi(), create, memory=mem, apps=apps, direct=direct, vault=vault if vault.enabled else None, watcher=watcher,
        meeter=meeter, lights=lights_hub, spotify=spotify_hub,
        brief=lambda: recall.opening_brief(recall_cfg, mem.transcripts if mem is not None else None))
    if miniapp.configured().enabled:
        inlet._maker = apps_maker.ChatMaker(_Tunnel(), lambda coro, name: coro.close())
    ch = chief.make_chief(watcher=watcher)
    if ch is None:
        raise SystemExit("NO_CHIEF: the chief did not build with EVE_CHIEF=on")
    inlet.chief = ch
    notes.append("lent: " + ", ".join(n for n, v in (("vault", vault.enabled), ("watcher", watcher),
                                                        ("apps", apps), ("meet", meeter), ("lights", lights_hub),
                                                        ("spotify", spotify_hub), ("maker", inlet._maker),
                                                        ("chief", ch)) if v))
    return inlet, ch, notes


async def _ask(inlet: Any, live: Any, i: int, case: Mapping[str, Any]) -> Row:
    """One case as a fresh chat's first message, through TelegramInlet._think (the daily rundown's path as
    _answer_turn takes it), stopped at the first tool call."""
    from eve_bridge import chief_card, rundown
    from eve_bridge import telegram as tg

    text = str(case["text"])
    seen: list[tuple[str, dict[str, Any]]] = []

    async def record(name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
        seen.append((name, args))
        raise _Stopped

    inlet._tool = record

    async def no_owner_to_ask(name: str, args: Any, chat_id: int) -> None:
        return None                   # the taint gate passes the call on to the recorder, which runs nothing

    inlet._gate = no_owner_to_ask
    items = [tg.message_item("user", text)]
    daily = rundown.matches(text)
    if daily:
        skill = await asyncio.to_thread(rundown.context, inlet._vault.root if inlet._vault else None,
                                        datetime.now().astimezone())
        items = [tg.message_item("user", text), tg.message_item("developer", skill)]
    label = str(case["label"])
    try:
        await inlet._think(items, 0, daily=daily, tail=0)
    except _Stopped:
        pass
    except Exception as e:  # noqa: BLE001 — the type only: fail-closed in the bar
        return Row(i, label, False, (), False, seen[0][0] if seen else "", "", type(e).__name__)
    if not seen:
        return Row(i, label, False, (), False, "", "", "")
    name, args = seen[0]
    if name != "take_on":
        return Row(i, label, False, (), False, name, "", "")
    checks = args.get("done_checks") if isinstance(args.get("done_checks"), list) else []
    kinds = tuple(str(c.get("kind")) for c in checks if isinstance(c, dict))
    question = isinstance(args.get("question"), str) and bool(args["question"].strip())
    parsed = chief_card.parse(args, said=text, live=live)
    refused = parsed.reason if isinstance(parsed, chief_card.Refusal) else ""
    return Row(i, label, True, kinds, question, name, refused, "")


def capture(data_dir: Path) -> int:
    """Score the blind set once. Refuses when it was scored, or when its sha256 is not the committed one."""
    path, out = data_dir / HOLDOUT, data_dir / RESULT
    if out.exists():
        print(f"ALREADY_SCORED: {out.name} holds the verdict; a blind set is scored exactly once")
        return 2
    digest = sha256(path)
    if digest != committed_sha(data_dir):
        print(f"SHA_MISMATCH: {path.name} is {digest}, the committed hash is {committed_sha(data_dir)}")
        return 2
    print(f"   {path.name} sha256 {digest} = {HOLDOUT_SHA}: ok")
    cases = json.loads(path.read_text(encoding="utf-8"))["cases"]
    with tempfile.TemporaryDirectory(prefix="chief-eval-") as tmp:
        scratch = Path(tmp)
        _daemon_environment(scratch)
        inlet, ch, notes = _brain(scratch)
        print("   brain: " + "; ".join(notes))
        from eve_bridge import spend

        async def run() -> list[Row]:
            parts, allowed, _ = await inlet._turn_parts(0)
            names = [t.get("name") for t in parts["watch_tools"]]
            print(f"   request: {len(allowed)} lent tool names; take_on offered: {'take_on' in names}; chief "
                  f"block in the instructions: {chief.instructions(ch.live) in parts['watch_block']}")
            rows = []
            t0 = time.monotonic()
            for i, case in enumerate(cases):
                rows.append(await _ask(inlet, ch.live, i, case))
            print(f"   {len(rows)} cases in {time.monotonic() - t0:.0f} s")
            return rows

        rows = asyncio.run(run())
        dispatched = sum(1 for e in ch.ledger.events() if e.get("e") == "dispatched")
        cost = spend.totals(spend.day_rows(date.today().isoformat()))
        print(f"   cost: ${cost['usd']:.4f} over {cost['calls']} model calls ({cost['unpriced']} unpriced)")
    bar = capture_bar(rows, cases, dispatched)
    report(bar, rows, cases)
    print(f"   one-yes-one-act tests for (d): PYTEST {ONE_YES_TESTS}")
    out.write_text(json.dumps({"scored": date.today().isoformat(), "holdout_sha256": digest,
                               "model": notes[0], "dispatched": dispatched, "usd": cost["usd"],
                               "shipped": bar["shipped"], "rows": [asdict(r) for r in rows]},
                              ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    print(f"   verdict kept in {out.name}")
    print(f"SHIPPED={bar['shipped']}")
    print(f"CAPTURE DECISION: {'ship' if bar['shipped'] else 'hold'}")
    print("CAPTURE_EVAL_COMPLETE")
    return 0


def check_default(data_dir: Path, shipped: Optional[bool] = None) -> int:
    """G7.3: chief.SHIPPED equals the bar recomputed from the kept verdict (offline, deterministic); with no
    verdict yet, SHIPPED is False. ``shipped`` is a test seam for chief.SHIPPED."""
    shipped = chief.SHIPPED if shipped is None else shipped
    out = data_dir / RESULT
    if not out.exists():
        if shipped:
            print("DEFAULT_MISMATCH: no blind set was scored, and chief.SHIPPED is True")
            return 1
        print("   not scored yet: chief.SHIPPED is False")
        print("DEFAULT_CONSISTENT")
        return 0
    kept = json.loads(out.read_text(encoding="utf-8"))
    digest = sha256(data_dir / HOLDOUT)
    if not digest == kept.get("holdout_sha256") == committed_sha(data_dir):
        print(f"SHA_MISMATCH: the set is {digest}, the verdict scored {kept.get('holdout_sha256')}, the committed "
              f"hash is {committed_sha(data_dir)}")
        return 1
    cases = json.loads((data_dir / HOLDOUT).read_text(encoding="utf-8"))["cases"]
    rows = [Row(**{**r, "kinds": tuple(r["kinds"])}) for r in kept["rows"]]
    if [r.label for r in rows] != [str(c["label"]) for c in cases] or [r.i for r in rows] != list(range(len(cases))):
        print("RESULT_MISMATCH: the kept rows are not the set's cases in order")
        return 1
    bar = capture_bar(rows, cases, int(kept["dispatched"]))
    if bar["shipped"] != kept["shipped"]:
        print(f"RESULT_MISMATCH: the kept verdict says {kept['shipped']}, the bar recomputed says {bar['shipped']}")
        return 1
    print(f"   scored {kept['scored']}: (a) {_pct(bar['a_rate'])} (b) {_pct(bar['b_rate'])} (c) {_pct(bar['c_rate'])} "
          f"(d) {bar['dispatched']} -> {'ship' if bar['shipped'] else 'hold'}")
    if shipped != bar["shipped"]:
        print(f"DEFAULT_MISMATCH: the eval says {bar['shipped']}, chief.SHIPPED is {shipped}")
        return 1
    print("DEFAULT_CONSISTENT")
    return 0


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="chief_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("--capture", action="store_true", help="E1: the brain's take_on on the blind capture set")
    p.add_argument("--check-default", action="store_true",
                   help="assert chief.SHIPPED equals the kept verdict (offline; never scores)")
    p.add_argument("--data", default=str(DATA_DIR), help="the folder holding the blind set and its verdict")
    args = p.parse_args(argv)
    if not args.capture:
        p.error("only --capture exists so far")
    data_dir = Path(args.data)
    if args.check_default:
        return check_default(data_dir)
    return capture(data_dir)


if __name__ == "__main__":
    sys.exit(main())
