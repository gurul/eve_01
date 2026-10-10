"""The request classifier (task_router.py) and its reflex tier in ComputerAgent.

The scored evidence is tools/route_eval.py on tests/fixtures/routes; these tests pin the rules'
behaviour on the traps that evidence found, and the agent's handling of a reflex.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

from test_computer_agent import FakeClient, FakeWorker, _agent, _message, _response

from eve_bridge import task_router as tr
from eve_bridge.computer_agent import AgentConfig, configured

APPS = ["Spotify", "TV", "Terminal", "Safari", "Slack", "Google Chrome", "System Settings", "Notes", "Mail", "Music",
        "Reminders", "Preview", "Calendar"]


def plan(goal: str, **kw) -> tr.Plan:
    return tr.classify(goal, apps=APPS, **kw)


def test_a_bare_launch_of_an_installed_app_is_a_complete_reflex() -> None:
    for goal, app in (("Open Spotify.", "Spotify"), ("open up Spotify on my laptop", "Spotify"),
                      ("Open up Apple TV on my Mac.", "TV"), ("Can you fire up Slack for me?", "Slack"),
                      ("Would you mind opening Preview?", "Preview"), ("fire up system settings", "System Settings"),
                      ("Open Spotify on the Mac and bring it to the front so you can start listening.", "Spotify")):
        p = plan(goal)
        assert (p.kind, p.tiers, p.app) == ("launch", ("reflex",), app), (goal, p)
        assert p.complete and p.code() == f"open_app({app!r}, 4.0)" and p.sentence() == f"Opened {app}."


def test_a_launch_with_more_to_do_hands_the_rest_to_the_planner() -> None:
    p = plan('Open Spotify and play "Don Toliver"')
    assert (p.kind, p.tiers, p.app) == ("launch", ("reflex", "astra"), "Spotify") and "play" in p.rest
    assert not p.complete


def test_what_looks_like_a_launch_and_is_not() -> None:
    for goal in ("Open the downloads folder.", "Open notes from yesterday's meeting.", "Show me photos of the trip.",
                 "Open a new Astra 6 session.", "Open Figma.", "Play Spotify", "Pause Spotify."):
        p = plan(goal)
        assert "reflex" not in p.tiers, (goal, p)
    for goal in ("Force quit Spotify.", "Open Mail and send the draft to Sam.", "Is Slack open?",
                 "Open Reminders and add a reminder to call the dentist.",
                 "Can you open the Music app and tell me what's playing?",
                 'Write this into the Terminal right now: "hello"'):
        assert plan(goal).tiers == ("astra",), goal            # consequential, typing, or needs eyes: planner only


def test_an_explicit_web_search_is_a_reflex_with_the_query_found_by_code() -> None:
    p = plan("Search for the closest gym in San Francisco using Google.")
    assert p.kind == "search" and p.complete and p.query == "the closest gym in San Francisco"
    assert p.code() == "open_url('https://www.google.com/search?q=the+closest+gym+in+San+Francisco')"
    assert plan("Pull up an article on Google on my laptop right now about the two leaders meeting.").query == \
        "an article about the two leaders meeting"
    told = plan("In Safari, search Google for the weather in San Francisco and tell me the temperature you see.")
    assert told.tiers == ("reflex", "astra") and told.browser == "Safari" and told.query == "the weather in San Francisco"
    assert "app='Safari'" in told.code()
    # a web search is harmless whatever its query says …
    assert plan("Google how to reset a Casio watch.").complete
    assert plan("search the web for how to delete a git branch").query == "how to delete a git branch"
    # … but a risky ACTION after the search is the planner's
    assert plan("Search for wireless earbuds and buy the cheapest pair.").tiers == ("astra",)


def test_what_looks_like_a_web_search_and_is_not() -> None:
    for goal in ("Search Spotify for jazz.", "Search for the budget thread in Slack.", "look up Priya Raman in my contacts",
                 "Pull up the budget.", "Find my keys.", "Search up some cool shit for me on Google right now.",
                 "Pull up something cool on Google."):
        assert "reflex" not in plan(goal).tiers, goal
    assert plan("Search Google for calendar templates for 2027.").complete      # an app's name as a query word


def test_a_model_is_asked_only_after_the_gates_and_may_only_add_a_bare_launch() -> None:
    asked: list[str] = []

    def says(app: str):
        def model(text: str, apps: list[str]) -> str:
            asked.append(text)
            return app
        return model

    p = plan("Notes, please.", model=says("Notes"))
    assert (p.kind, p.tiers, p.app) == ("launch", ("reflex",), "Notes") and "model" in p.reasons[0]
    assert plan("get Spotify going and delete my playlist", model=says("Spotify")).tiers == ("astra",)   # the code gate first
    assert asked == ["Notes, please."]
    assert plan("Open Spotify.", model=says("Notes")).app == "Spotify" and asked == ["Notes, please."]   # rules fire: not asked
    assert "reflex" not in plan("Take me to Figma.", model=says("Figma")).tiers        # not installed: not the model's call
    assert "reflex" not in plan("Take me somewhere nice.", model=says("")).tiers       # the model abstained

    def boom(text: str, apps: list[str]) -> str:
        raise RuntimeError("network down")
    assert plan("Notes, please.", model=boom).tiers == ("lane", "astra")
    assert tr.find_app_mention("put on apple tv", APPS) == "TV" and tr.find_app_mention("chrome please", APPS) == "Google Chrome"


def test_jev_is_asked_in_its_own_idiom_and_gated_by_absolute_answers() -> None:
    from eve_bridge import typed_ask as ta

    seen: dict = {}

    def predict(state, questions):
        seen.update(state=state, questions=questions)
        return {"answers": {"launch_only": {"type": "noul", "noul": 0.93},
                            "app": {"choice": "app3", "probabilities": {"app3": 0.97, "none": 0.01}},
                            "web_search": {"type": "noul", "noul": 0.02},
                            "risky_action": {"type": "noul", "noul": 0.03}}}

    asker = ta.make_jev_request_asker(predict, lambda: 0.0)
    assert asker("Take me to Safari.", APPS) == "Safari"                       # APPS[3]
    q = seen["questions"]
    assert {k: v["type"] for k, v in q.items()} == {"launch_only": "noul", "app": "choice", "web_search": "noul",
                                                    "risky_action": "noul"}      # absolute gates, one request
    assert q["app"]["criteria"]["app3"] == "Safari" and "none" in q["app"]["criteria"]   # the REAL installed list
    assert seen["state"]["request"] == "Take me to Safari." and len(seen["state"]) == 2  # a small, structured state
    risky = ta.RequestAnswer(launch_only=0.99, app="Mail", p_app=0.99, web_search=0.0, risky=0.6)
    assert ta.decide_request(risky, ta.JEV_REQUEST_GATES) == "other"            # risk outranks everything
    unsure = ta.RequestAnswer(launch_only=0.6, app="Mail", p_app=0.99, web_search=0.0, risky=0.0)
    assert ta.decide_request(unsure, ta.JEV_REQUEST_GATES) == "other"           # under the gate: abstain
    assert ta.make_jev_request_asker(lambda s, q: (_ for _ in ()).throw(OSError("down")), lambda: 0.0)("x", APPS) == ""


def test_each_model_is_asked_differently_for_a_head_pose() -> None:
    from eve_bridge import typed_ask as ta

    laya, jevq = ta.LAYA_HEAD_QUESTIONS, ta.JEV_HEAD_QUESTIONS
    # laya: relative choices only, short options, the gate is a choice too
    assert all(q["type"] == "choice" for q in laya.values())
    assert max(len(str(v).split()) for q in laya.values() for v in q["criteria"].values()) <= 6
    assert max(len(q["criteria"]) for q in laya.values()) <= 7
    # jev: an absolute noul gate, with literal instructions that say what does NOT count
    assert jevq["is_head"]["type"] == "noul" and "scroll" in jevq["is_head"]["instructions"]
    assert "none" in jevq["direction"]["criteria"]
    a = ta.HeadAnswer(gate=0.9, direction="left", p_direction=0.9, size="little", p_size=0.8)
    assert a.pose() == "bit_left" and ta.Gates(0.3, 0.7).decide(a) == "bit_left"
    assert ta.Gates(0.95, 0.7).decide(a) == "none"
    assert ta.HeadAnswer(0.9, "up", 0.9, "far", 0.9).pose() == "up" and ta.HeadAnswer(0.9, "desk", 0.9, "far", 0.9).pose() == "desk"
    fitted = ta.fit_gates([a, ta.HeadAnswer(0.4, "down", 0.95, "normal", 0.9)], ["bit_left", "none"])
    assert fitted.decide(a) == "bit_left" and fitted.gate > 0.4                # a false move is never bought


def test_installed_apps_lists_real_names_once() -> None:
    names = tr.installed_apps(dirs=("/x",), listdir=lambda d: ["Spotify.app", "TV.app", "notes.txt"])
    assert names == ("Spotify", "TV")
    assert tr.installed_apps(dirs=("/missing",), listdir=lambda d: (_ for _ in ()).throw(OSError())) == ()


# ---- the agent ----------------------------------------------------------------------------------


def _reflex_worker(sentence: str) -> FakeWorker:
    code = "open_app('Spotify', 4.0)"
    return FakeWorker({code: [{"type": "input_text", "text": sentence}, {"type": "input_text", "text": "[after] x"}]})


def test_a_complete_reflex_makes_zero_planner_calls(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(tr, "installed_apps", lambda *a, **k: tuple(APPS))
    client = FakeClient([])
    worker = _reflex_worker("opened Spotify (frontmost after 0.4 s)")
    cfg = AgentConfig(runs_dir=tmp_path / "runs", reflexes=True, lane_first=False)
    a, events = _agent(client, worker, tmp_path, config=cfg)
    assert asyncio.run(a.run("Open up Spotify on my laptop.")) == "Opened Spotify."
    assert client.calls == 0 and worker.executed == ["open_app('Spotify', 4.0)"]
    assert [e.kind for e in events][-1] == "final"


def test_a_reflex_that_did_not_confirm_leaves_the_task_to_the_planner(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(tr, "installed_apps", lambda *a, **k: tuple(APPS))
    client = FakeClient([_response("r1", _message("Spotify is open."))])
    worker = _reflex_worker("opened Spotify but Warp is still frontmost after 8.0 s")
    cfg = AgentConfig(runs_dir=tmp_path / "runs", reflexes=True, lane_first=False, verify=False)
    a, _ = _agent(client, worker, tmp_path, config=cfg)
    assert asyncio.run(a.run("Open Spotify.")) == "Spotify is open."
    assert client.calls == 1 and "[note]" not in client.requests[0]["input"][0]["content"][0]["text"]


def test_a_partial_reflex_tells_the_planner_what_is_done_and_what_is_left(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(tr, "installed_apps", lambda *a, **k: tuple(APPS))
    client = FakeClient([_response("r1", _message("Playing."))])
    worker = _reflex_worker("opened Spotify (frontmost after 0.4 s)")
    cfg = AgentConfig(runs_dir=tmp_path / "runs", reflexes=True, lane_first=False, verify=False)
    a, _ = _agent(client, worker, tmp_path, config=cfg)
    assert asyncio.run(a.run("Open Spotify and play some jazz")) == "Playing."
    first = client.requests[0]["input"][0]["content"][0]["text"]
    assert "[note] Before you started, a reflex already opened Spotify." in first and "play some jazz" in first


def test_reflexes_off_is_the_old_path_and_the_default_is_the_evals(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _message("done"))])
    worker = FakeWorker()
    cfg = AgentConfig(runs_dir=tmp_path / "runs", reflexes=False, lane_first=False)
    a, _ = _agent(client, worker, tmp_path, config=cfg)
    assert asyncio.run(a.run("Open Spotify.")) == "done" and worker.executed == [] and client.calls == 1
    assert AgentConfig().reflexes is tr.REFLEX_DEFAULT and configured({}).reflexes is tr.REFLEX_DEFAULT
    assert configured({"EVE_REFLEXES": "1"}).reflexes is True and configured({"EVE_REFLEXES": "off"}).reflexes is False


def test_the_routing_doc_states_the_shipped_defaults_and_names_every_knob() -> None:
    import re

    from eve_bridge.fast_lane import DEFAULT_DECIDE, LANE_FIRST_DEFAULT

    root = Path(__file__).resolve().parents[2]
    text = (root / "docs" / "stackchan" / "routing.md").read_text(encoding="utf-8")

    def row(knob: str) -> str:
        m = re.search(rf"^\| `{knob}` \| `([^`]*)` \|", text, re.M)
        assert m, knob
        return m.group(1)

    assert row("EVE_REFLEXES") == ("1" if tr.REFLEX_DEFAULT else "0")
    assert row("EVE_ROUTER_MODEL") == tr.ROUTER_MODEL_DEFAULT
    assert row("EVE_HEAD_MODEL") == "off" and "already done" in text
    assert row("EVE_LANE_FIRST") == ("1" if LANE_FIRST_DEFAULT else "0")
    assert row("EVE_FAST_LANE_DECIDE") == DEFAULT_DECIDE and row("EVE_DECIDER") == "unset"
    for phrase in ("absolute noul gate", "laya ranks", "Jev judges", "holdout 3", "thin but real", "typed_ask.py",
                   "tools/route_eval.py", "tools/head_eval.py", "unsafe"):
        assert phrase in text, phrase
    voice = (root / "docs" / "stackchan" / "voice.md").read_text(encoding="utf-8")
    assert "EVE_REFLEXES" in voice and "EVE_ROUTER_MODEL" in voice and "routing.md" in voice
    assert "routing.md" in (root / "README.md").read_text(encoding="utf-8")


def test_is_question_is_one_rule_for_both_routers() -> None:
    from eve_bridge.lane_router import is_question

    assert is_question("what's playing?") and is_question("is Slack open") and is_question("open it?")
    assert not is_question("open it?", polite=True)          # "can you open it?" is a request
    assert not is_question("open Spotify")


def test_app_names_said_as_one_word_and_every_mention() -> None:
    assert tr.match_app("photobooth", ["Photo Booth", "Photos"]) == "Photo Booth"
    assert tr.match_app("photo booth", ["Photo Booth", "Photos"]) == "Photo Booth"
    assert tr.match_app("photos", ["Photo Booth", "Photos"]) == "Photos"
    assert tr.match_app("photobooth", ["Photo Booth", "Photob Ooth"]) == ""       # two apps have those letters
    assert tr.app_mentions("open spotify and chrome", ["Spotify", "Google Chrome"]) == ["Spotify", "Google Chrome"]
    assert tr.app_mentions("take a photo in photo booth", ["Photo Booth", "Photos"]) == ["Photo Booth"]
    assert tr.find_app_mention("open spotify and chrome", ["Spotify", "Google Chrome"]) == ""
    assert tr.classify("open photobooth", apps=("Photo Booth", "Photos")).app == "Photo Booth"
