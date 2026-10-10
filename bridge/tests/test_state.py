from eve_bridge.state import (
    NON_WAITING_NOTIFICATION_KINDS,
    State,
    notification_waits,
)


def test_session_lifecycle():
    s = State()
    s.session_start("a", transcript_path="/tmp/a.jsonl", cwd="/tmp")
    s.session_start("b")
    assert s.total == 2
    s.session_end("a")
    assert s.total == 1
    assert "b" in s.sessions


def test_turn_running_count():
    s = State()
    s.session_start("x")
    s.session_start("y")
    s.turn_begin("x")
    assert s.running_count == 1
    s.turn_begin("y")
    assert s.running_count == 2
    s.turn_end("x")
    assert s.running_count == 1


def test_entries_newest_first_and_capped():
    s = State()
    for i in range(20):
        s.add_entry(f"line {i}")
    assert len(s.entries) == State.MAX_ENTRIES
    # newest first
    assert s.entries[0].text == "line 19"


def test_tokens_setter():
    s = State()
    s.set_tokens(123, 45)
    assert s.tokens_cumulative == 123
    assert s.tokens_today == 45
    # Cost params default to 0 when omitted
    assert s.cost_cumulative == 0.0
    assert s.cost_today == 0.0


def test_tokens_setter_with_cost():
    s = State()
    s.set_tokens(1000, 200, cost_cumulative=12.34, cost_today=2.50)
    assert s.cost_cumulative == 12.34
    assert s.cost_today == 2.50


def test_attention_cwd_falls_back_to_newest_needs_input():
    s = State()
    s.session_start("a", cwd="/repos/alpha")
    s.session_start("b", cwd="/repos/beta")
    s.needs_input("a")
    s.needs_input("b")   # later flag wins
    assert s.attention_cwd() == "/repos/beta"


def test_attention_cwd_empty_when_nothing_waits():
    s = State()
    s.session_start("a", cwd="/repos/alpha")
    assert s.attention_cwd() == ""
    s.needs_input("a")
    s.input_received("a")
    assert s.attention_cwd() == ""


# ---- notification kinds: which ones mean "Claude needs you" ----
# Waiting kinds light the attention pose; an idle reminder must not take over
# a live voice conversation (firmware derive(): Claude idle -> the pet sleeps).


def test_notification_kind_permission_prompt_waits():
    assert notification_waits("permission_prompt") is True


def test_notification_kind_elicitation_dialog_waits():
    assert notification_waits("elicitation_dialog") is True


def test_notification_kind_idle_prompt_does_not_wait():
    assert notification_waits("idle_prompt") is False


def test_notification_kind_idle_does_not_wait():
    assert notification_waits("idle") is False


def test_notification_kind_auth_success_does_not_wait():
    assert notification_waits("auth_success") is False


def test_notification_kind_elicitation_response_does_not_wait():
    assert notification_waits("elicitation_response") is False


def test_notification_kind_missing_or_empty_waits():
    # No kind at all: keep the old, conservative behavior.
    assert notification_waits(None) is True
    assert notification_waits("") is True


def test_notification_kind_unknown_waits():
    # A kind Claude Code adds later may be blocking: still get attention.
    assert notification_waits("some_future_kind") is True


def test_notification_kind_sets_are_exact_and_disjoint():
    waiting = {"permission_prompt", "elicitation_dialog"}
    assert NON_WAITING_NOTIFICATION_KINDS == {
        "idle_prompt", "idle", "auth_success", "elicitation_response",
    }
    assert not waiting & NON_WAITING_NOTIFICATION_KINDS
    assert all(notification_waits(k) for k in waiting)
    assert not any(notification_waits(k) for k in NON_WAITING_NOTIFICATION_KINDS)


def test_notification_kind_drives_waiting_count():
    s = State()
    s.session_start("x")
    for kind in NON_WAITING_NOTIFICATION_KINDS:
        if notification_waits(kind):
            s.needs_input("x")
        assert s.waiting_count == 0, kind
    for kind in ("permission_prompt", "elicitation_dialog", None, "some_future_kind"):
        s.input_received("x")
        assert s.waiting_count == 0
        if notification_waits(kind):
            s.needs_input("x")
        assert s.waiting_count == 1, kind
