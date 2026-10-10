"""The fast head-move path in the voice session: a typed model picks the pose, the head turns at once,
and the backend's own move_head for the same turn never turns it a second time."""

from __future__ import annotations

import asyncio

from eve_bridge import voice_agent as va
from eve_bridge.head import POSE_MOVES, Head
from eve_bridge.typed_ask import JEV_HEAD_GATES, POSES, HeadAnswer, make_jev_head_asker


class Session(va.VoiceSession):
    """A VoiceSession with no Live connection: only the pieces the fast path touches."""

    def __init__(self, pose: str, head: Head) -> None:       # noqa: D107 — deliberately not calling super().__init__
        self.head_pose = self._asker
        self._pose = pose
        self.asked: list[str] = []
        self.head = head
        self._think_aloud = None
        self._fast_head = None
        self._last_head_tool_at = float("-inf")
        self._user_turn_started_at = 10.0
        self._ended = asyncio.Event()
        self._t = 10.0
        self._clock = self._tick
        self.tool_calls = []
        self._last_activity = 0.0

    def _tick(self) -> float:
        self._t += 0.01
        return self._t

    async def _asker(self, text: str) -> str:
        self.asked.append(text)
        if self._pose == "boom":
            raise RuntimeError("network down")
        return self._pose


def _head(sent: list[dict]) -> Head:
    async def send(cmd: dict) -> bool:
        sent.append(cmd)
        return True
    return Head(send)


def test_every_pose_has_a_move_and_the_words_filter_is_a_filter_not_a_judge() -> None:
    assert set(POSE_MOVES) == set(POSES)
    assert POSE_MOVES["left"]["yaw"] < 0 < POSE_MOVES["right"]["yaw"]           # the robot's own left is negative yaw
    assert POSE_MOVES["bit_left"]["relative"] and not POSE_MOVES["far_left"]["relative"]
    for text in ("look left", "a bit lower please", "face me", "turn your head all the way right", "scroll down"):
        assert va.HEAD_WORDS.search(text), text                # "scroll down" passes the filter: the MODEL says no
    for text in ("what's the weather", "open spotify", "tell me a joke"):
        assert not va.HEAD_WORDS.search(text), text             # these never leave the Mac for a pose


def test_a_sure_pose_turns_the_head_now_and_the_backend_does_not_turn_it_again() -> None:
    sent: list[dict] = []
    s = Session("bit_left", _head(sent))

    async def go() -> dict:
        await s._fast_head_move("a bit more to the left", 10.0)
        assert [c["yaw"] for c in sent] == [-15]                # one relative nudge from 0
        # the voice also delegated the same turn, and the backend now calls move_head: it must NOT move again
        await s._tool_move_for_test({"yaw": -15, "relative": True})
        return s._fast_head[1]

    async def tool(args: dict) -> None:
        fast = s._fast_head
        if fast is not None and fast[0] >= s._user_turn_started_at:
            return
        await s._move_head(args)
    s._tool_move_for_test = tool                                # the branch in _tool, without a Live connection
    result = asyncio.run(go())
    assert [c["yaw"] for c in sent] == [-15] and result["ok"] and "already done" in result["note"]
    # a NEW turn is a new request: the backend's move for it runs
    s._user_turn_started_at = 99.0
    asyncio.run(tool({"yaw": -15, "relative": True}))
    assert [c["yaw"] for c in sent] == [-15, -30]


def test_none_a_failure_and_a_slow_answer_all_leave_the_turn_to_the_ordinary_path() -> None:
    for pose in ("none", "boom", "nonsense"):
        sent: list[dict] = []
        s = Session(pose, _head(sent))
        asyncio.run(s._fast_head_move("look up the weather in Paris", 10.0))
        assert sent == [] and s._fast_head is None, pose
    sent = []
    late = Session("left", _head(sent))
    late._last_head_tool_at = 50.0                              # the backend already turned the head for this turn
    asyncio.run(late._fast_head_move("look left", 10.0))
    assert sent == []


def test_the_asker_applies_the_shipped_gates() -> None:
    def predict(state, questions):
        assert "owner_said" in state and questions["is_head"]["type"] == "noul"
        return {"answers": {"is_head": {"type": "noul", "noul": 0.9},
                            "direction": {"choice": "left", "probabilities": {"left": 0.95}},
                            "size": {"choice": "little", "probabilities": {"little": 0.9}}}}
    assert make_jev_head_asker(predict, lambda: 0.0)("a little to the left") == "bit_left"
    unsure = HeadAnswer(gate=0.2, direction="left", p_direction=0.95, size="normal", p_size=0.9)
    assert JEV_HEAD_GATES.decide(unsure) == "none"
    assert make_jev_head_asker(lambda s, q: (_ for _ in ()).throw(OSError()), lambda: 0.0)("look left") == "none"
