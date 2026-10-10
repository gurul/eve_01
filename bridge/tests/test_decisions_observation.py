"""Fresh readout evidence, state admission and privacy across Decisions observations."""
from __future__ import annotations

import json
from dataclasses import replace

import pytest
from test_ax_candidates import node, raw
from test_decision_lane import Client, Senses
from test_fast_lane import Effectors, calendar, cand

from eve_bridge import decision_lane as lane
from eve_bridge import decisions
from eve_bridge.ax_candidates import MAX_VALUE_CHARS, snapshot_from_dict, snapshot_from_raw


def readout(value, *, secure=False, framed=True):
    display = node("AXStaticText", description="Result display", value=value,
                   frame=(0, 0, 80, 20) if framed else None)
    if secure:
        display = node("AXTextField", subrole="AXSecureTextField", title="Secret field",
                       focused=True, children=[display], value="secret-editable")
    return snapshot_from_raw(raw(display, node("AXButton", "Seven", frame=(0, 30, 60, 30))))


def test_readonly_value_with_descriptive_label_survives_into_model_evidence():
    before, after = readout("0"), readout("7")
    assert before.elements == after.elements  # existing Candidate value/ID contract preserved
    assert before._readouts == (("0", "0"),) and after._readouts == (("0", "7"),)
    data = lane.evidence("click seven", after)
    assert data["readouts"] == [{"id": "0", "label": "Result display", "text": "7"}]
    assert "Result display" in data["context"]
    assert not lane.same_state(before, after)
    assert lane.same_state(after, replace(after, seq=after.seq + 1, secs=999))


def test_existing_snapshot_schema_and_candidate_ids_are_unchanged():
    snapshot = readout("7")
    serialized = snapshot.to_dict()
    assert "_readouts" not in serialized and "readouts" not in serialized
    reconstructed = snapshot_from_dict(serialized)
    assert reconstructed.to_dict() == serialized and reconstructed.elements == snapshot.elements
    assert reconstructed._readouts == ()  # archived schema has no fresh transport readings


def test_readouts_require_visible_geometry_and_are_bounded_with_truncation_marker():
    assert readout("7", framed=False)._readouts == ()
    text = lane.evidence("observe", readout("x" * 100))["readouts"][0]["text"]
    assert text == "x" * (MAX_VALUE_CHARS - 1) + "…"


@pytest.mark.parametrize("value", ["secret-static", "selected", "checked"])
def test_secure_field_and_descendant_text_do_not_reach_evidence(value):
    snapshot = readout(value, secure=True)
    assert all(c.secure for c in snapshot.elements if c.role != "button")
    assert snapshot._readouts == ()
    payload = json.dumps(lane.evidence("click seven", snapshot))
    assert value not in payload and "secret-editable" not in payload and "Secret field" not in payload
    assert "Seven" in payload  # positive control: useful nonsecure controls still included


def test_secure_descendant_cannot_supply_label_to_nonsecure_parent_button():
    snapshot = snapshot_from_raw(raw(node("AXButton", children=[
        node("AXSecureTextField", value="secret-descendant", children=[
            node("AXStaticText", value="secret-static")])]), node("AXButton", "Next")))
    assert not any(c.role == "button" and c.label.startswith("secret") for c in snapshot.elements)
    assert "secret" not in json.dumps(lane.evidence("click next", snapshot))


def test_editable_value_and_untrusted_context_are_excluded_but_readonly_context_is_kept():
    snapshot = snapshot_from_raw(raw(node("AXTextField", "Query", value="private draft"),
                                     node("AXStaticText", value="Results loaded", frame=(0, 30, 60, 20))))
    snapshot = replace(snapshot, context_lines=(*snapshot.context_lines, "private draft", "invented instructions"))
    payload = lane.evidence("click next", snapshot)
    assert "Results loaded" in payload["context"]
    assert "private draft" not in json.dumps(payload) and "invented instructions" not in json.dumps(payload)


def test_labelled_static_value_inside_editable_field_is_not_a_readout():
    snapshot = snapshot_from_raw(raw(node("AXTextField", "Query", children=[
        node("AXStaticText", description="Draft backing text", value="private draft")]),
        node("AXStaticText", description="Result", value="7", frame=(0, 30, 60, 20))))
    data = lane.evidence("observe", snapshot)
    assert data["readouts"] == [{"id": "2", "label": "Result", "text": "7"}]
    assert "private draft" not in json.dumps(data)


def test_readout_change_before_action_is_stale_with_zero_input():
    before, current = readout("0"), readout("7")
    effectors, client = Effectors(), Client(target="1")
    result = lane.route("click seven", senses=Senses([before, current]), effectors=effectors,
                        client=client, apps=[])
    assert result.reason == "stale_observation" and result.status == "none"
    assert not effectors.clicks and len(client.calls) == 1
    assert "decisions admission: AX state changed" in result.log_lines


@pytest.mark.parametrize("changed", [False, True])
def test_changed_or_unchanged_readout_never_substitutes_for_verification(changed):
    before, after = readout("0"), readout("7" if changed else "0")
    client, effectors = Client(target="1", verified=0.2), Effectors()
    result = lane.route("click seven", senses=Senses([before, before, after]), effectors=effectors,
                        client=client, apps=[])
    assert result.status == "partial" and result.reason == "unverified"
    assert effectors.clicks == result.clicked == ["Seven"]
    assert client.calls[-1][0]["after"]["readouts"][0]["text"] == ("7" if changed else "0")
    assert any("verified=0.200" in line for line in result.log_lines)


def test_verifier_can_accept_observed_noneditable_result_at_original_threshold():
    class Observed(Client):
        def evaluate(self, evidence, images, questions):
            if len(questions) == 1:
                self.calls.append((evidence, images, questions))
                expected = evidence["before"]["readouts"][0]["text"] == "0"
                expected &= evidence["after"]["readouts"][0]["text"] == "7"
                return decisions.Answer({"verified": 0.99 if expected else 0.1})
            return super().evaluate(evidence, images, questions)

    before, after = readout("0"), readout("7")
    client, effectors = Observed(target="1"), Effectors()
    result = lane.route("click seven", senses=Senses([before, before, after]), effectors=effectors,
                        client=client, apps=[])
    assert result.status == "complete" and len(client.calls) == 2 and effectors.clicks == ["Seven"]
    assert lane.Gates().verified == 0.95 and lane.Gates().risk_max == 0.05


def test_post_action_focus_loss_retains_partial_input_without_verifier():
    class LostFocus(Senses):
        def frontmost_pid(self):
            return 42 if self.calls < 3 else 100

    client, effectors = Client(), Effectors()
    result = lane.route("show the next seven days", senses=LostFocus([calendar()]),
                        effectors=effectors, client=client, apps=[])
    assert result.status == "partial" and result.reason == "unverified"
    assert result.clicked == effectors.clicks == ["Week"] and len(client.calls) == 1


def test_diagnostics_report_only_model_scores_not_model_error_payloads():
    secret = "private upstream request body"
    client = Client(error=secret)
    result = lane.route("show the next seven days", senses=Senses([calendar()]), effectors=Effectors(),
                        client=client, apps=[])
    assert result.reason == "decision_error" and secret not in " ".join(result.log_lines)
    assert "model_error" in " ".join(result.log_lines)


def test_capture_failure_reports_phase_without_private_exception_detail():
    class Broken(Senses):
        def observe(self):
            if self.calls == 2:
                raise RuntimeError("private capture path")
            return super().observe()

    result = lane.route("show the next seven days", senses=Broken([calendar()]), effectors=Effectors(),
                        client=Client(), apps=[])
    assert result.status == "partial" and result.clicked == ["Week"]
    assert "decisions verification observation: RuntimeError" in result.log_lines
    assert "private capture path" not in " ".join(result.log_lines)


def test_manual_readout_cannot_override_secure_or_editable_candidate():
    snapshot = calendar()
    secure = cand("7", "static text", "Secret", secure=True, actions=())
    editable = cand("8", "static text", "Draft", editable=True, actions=())
    snapshot = replace(snapshot, elements=(*snapshot.elements, secure, editable),
                       _readouts=(("7", "secure-value"), ("8", "editable-value"), ("9", "missing-value")))
    assert lane.evidence("observe", snapshot)["readouts"] == []
