"""lite_rtc end to end in one process: two real aiortc peers, the web app's side played by one (``Page``), the
Mac's by RtcHub with the unchanged LinkChat and PhoneCalls behind it (docs/plans/eve-lite.md, M6). A turn completes
over the ``link`` channel; a 1 kHz tone held on the page's microphone reaches the call's press; eve's answer
comes back as sound on the audio track with its caption. Then the hub: no ICE server at all, the ``.local``
rewrite (an IPv4 peer only), one peer per device with the newest offer winning, at most four, revoking a device,
refusing what is not an offer, and closing a peer that never connects.

No ICE server on either side, so nothing here leaves the Mac; the peers meet on its own interfaces."""
from __future__ import annotations

import asyncio
import fractions
import json
import os
import re
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Optional

import numpy as np
import pytest
from test_link_chat import FakeInlet, events, store_at, two_lines
from test_lite_link_channel import FakeChannel, answers, door, req, until
from test_phone_call import Brain, Voice

from eve_bridge import link_chat, lite_rtc, phone_call
from eve_bridge.lite_rtc import RtcError, RtcHub, rewrite_local_candidates
from eve_bridge.phone_call import PhoneCalls

aiortc = pytest.importorskip("aiortc")
av = pytest.importorskip("av")
from aiortc.mediastreams import MediaStreamError, MediaStreamTrack  # noqa: E402

TURN, MESSAGES = link_chat.TURN_PATH, link_chat.MESSAGES_PATH


class Tone(MediaStreamTrack):
    """The page's microphone: a sine at ``hz``, as a browser's track reaches aiortc (48 kHz stereo 16-bit), 20 ms
    frames in real time."""

    kind = "audio"

    def __init__(self, hz: float = 1000.0, amp: int = 8000) -> None:
        super().__init__()
        self.hz, self.amp = hz, amp
        self._n = 0
        self._start: Optional[float] = None

    async def recv(self) -> Any:
        if self.readyState != "live":
            raise MediaStreamError
        if self._start is None:
            self._start = time.monotonic()
        else:
            wait = self._start + self._n / 48000 - time.monotonic()
            if wait > 0:
                await asyncio.sleep(wait)
        t = (np.arange(960) + self._n) / 48000
        mono = (np.sin(2 * np.pi * self.hz * t) * self.amp).astype(np.int16)
        frame = av.AudioFrame.from_ndarray(np.stack([mono, mono], axis=1).reshape(1, -1), format="s16",
                                           layout="stereo")
        frame.sample_rate, frame.pts, frame.time_base = 48000, self._n, fractions.Fraction(1, 48000)
        self._n += 960
        return frame


class Page:
    """The web app, as an aiortc peer: the ``link`` channel made before the offer, one sendrecv audio transceiver
    with no track (voice mode puts the microphone on it later with replaceTrack, no renegotiation), and the ``call``
    channel opened on demand. ``heard`` is the loudest sample of each frame of eve's voice that reached it."""

    def __init__(self) -> None:
        self.pc = aiortc.RTCPeerConnection(aiortc.RTCConfiguration(iceServers=[]))
        self.link = self.pc.createDataChannel("link")
        self.audio = self.pc.addTransceiver("audio", direction="sendrecv")
        self.link_got: list[Any] = []
        self.call: Any = None
        self.call_got: list[dict[str, Any]] = []
        self.heard: list[float] = []
        self._reading: Optional[asyncio.Task[Any]] = None
        self.link.on("message", self.link_got.append)
        self.pc.on("track", self._track)

    def _track(self, track: Any) -> None:
        if track.kind == "audio":
            self._reading = asyncio.ensure_future(self._listen(track))

    async def _listen(self, track: Any) -> None:
        while True:
            try:
                frame = await track.recv()
            except MediaStreamError:
                return
            self.heard.append(float(np.abs(frame.to_ndarray()).max()))

    async def offer(self) -> str:
        await self.pc.setLocalDescription(await self.pc.createOffer())
        return self.pc.localDescription.sdp

    async def accept(self, sdp: str) -> None:
        await self.pc.setRemoteDescription(aiortc.RTCSessionDescription(sdp=sdp, type="answer"))

    def open_call(self) -> None:
        self.call = self.pc.createDataChannel("call")
        self.call.on("message", lambda m: self.call_got.append(json.loads(m)))

    def say(self, msg: dict[str, Any]) -> None:
        self.call.send(json.dumps(msg))

    async def close(self) -> None:
        await self.pc.close()
        if self._reading is not None:
            self._reading.cancel()


class ToneVoice(Voice):
    """test_phone_call.py's fake voice, keeping the press it was given, and speaking 0.6 s of a 440 Hz tone."""

    def __init__(self) -> None:
        super().__init__(text="what time is it")
        self.pcm: Optional[bytes] = None

    def transcribe(self, pcm: bytes) -> str:
        self.pcm = bytes(pcm)
        return super().transcribe(pcm)

    def speak_stream(self, text: str):
        self.spoken.append(text)
        t = np.arange(int(0.6 * lite_rtc.SAMPLE_RATE)) / lite_rtc.SAMPLE_RATE
        data = (np.sin(2 * np.pi * 440 * t) * 8000).astype(np.int16).tobytes()
        for i in range(0, len(data), 4800):
            yield data[i:i + 4800]


def one_candidate(sdp: str) -> str:
    """The offer with only its first host candidate. A peer that will never connect then has one pair to check:
    aioice starts one check per pair 20 ms apart, and a peer closed inside that loop leaves checks sending on closed
    sockets (an aioice quirk, harmless but noisy), so these tests keep the loop short and wait it out."""
    first = next(ln for ln in sdp.split("\r\n") if ln.startswith("a=candidate:") and " typ host" in ln)
    return "\r\n".join(ln for ln in sdp.split("\r\n") if not ln.startswith("a=candidate:") or ln == first)


def dominant_hz(pcm: bytes, rate: int = lite_rtc.SAMPLE_RATE) -> float:
    x = np.frombuffer(pcm, dtype=np.int16).astype(np.float64)
    spectrum = np.abs(np.fft.rfft(x * np.hanning(len(x))))
    return float(np.fft.rfftfreq(len(x), 1 / rate)[int(np.argmax(spectrum))])


# ---- the whole path ---------------------------------------------------------------------------------------------

def test_two_peers_complete_a_turn_over_link_and_a_held_tone_reaches_the_call(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        link = door(tmp_path, inlet)
        brain, voice = Brain(reply="It is three o'clock."), ToneVoice()
        calls = PhoneCalls(lambda _: None, brain, voice)
        hub = RtcHub(link, calls)
        page = Page()
        try:
            async with asyncio.timeout(60):
                got = await hub.answer("phone-1", await page.offer(), "127.0.0.1")
                assert got["voice"] is True
                assert "a=candidate:" in got["sdp"] and "typ srflx" not in got["sdp"]   # every candidate, no STUN
                assert "a=sendrecv" in got["sdp"]                                     # eve's voice on the page's audio
                await page.accept(got["sdp"])
                await until(lambda: page.link.readyState == "open", 20)

                # a turn over link
                page.link.send(json.dumps(req(1, "POST", TURN, {"text": "hello", "client_id": "loop-turn-1"})))
                await until(lambda: answers(page.link_got).get(1, {}).get("end"), 10)
                a = answers(page.link_got)[1]
                assert a["head"]["s"] == 200 and a["end"] == "end"
                assert events(a["body"])[:2] == (["First line.", "\n\nSecond line."], True)
                assert inlet.heard == [("hello", False, "loop-turn-1")]
                stats = hub.stats()
                assert stats["link_turns"] == 1 and stats["connected"] == 1 and stats["peers"] == 1

                # voice: the microphone goes on, the call channel opens, then a one-second press of a 1 kHz tone
                page.audio.sender.replaceTrack(Tone(1000.0))
                page.open_call()
                await until(lambda: {"type": "state", "state": "connected"} in page.call_got, 10)
                await until(lambda: page.call.readyState == "open", 10)
                await asyncio.sleep(0.5)                                              # the tone flows before the press
                page.say({"type": "talk"})
                await asyncio.sleep(1.0)
                page.say({"type": "done"})
                await until(lambda: voice.pcm is not None, 10)
                seconds = len(voice.pcm) / phone_call.BYTES_PER_SEC
                assert 1.0 <= seconds <= 2.5, seconds                                # the press, its pre- and post-roll
                assert abs(dominant_hz(voice.pcm) - 1000) <= 15                       # the tone, through Opus and back
                assert np.abs(np.frombuffer(voice.pcm, dtype=np.int16)).max() > 4000

                # the answer comes back as sound on the page's track, its caption as its sound starts
                await until(lambda: any(m.get("type") == "said" for m in page.call_got), 10)
                await until(lambda: max(page.heard, default=0.0) > 2000, 10)
                kinds = [m["type"] for m in page.call_got]
                assert kinds.index("heard") < kinds.index("said")
                assert {"type": "said", "text": "It is three o'clock."} in page.call_got
                assert brain.heard == ["what time is it"] and hub.stats()["calls"] == 1
                page.say({"type": "end"})
                await until(lambda: {"type": "ended", "reason": phone_call.HUNG_UP} in page.call_got, 10)
                await until(lambda: calls.active is None, 5)
        finally:
            await page.close()
            await hub.close()
            link.close()
        assert hub.peers == {}

    asyncio.run(go())


# ---- ICE, candidates and peers --------------------------------------------------------------------------------------

def test_the_peers_have_no_ice_server_at_all() -> None:
    assert lite_rtc.configuration().iceServers == []

    async def go() -> None:
        hub = RtcHub(None, None)
        page = Page()
        try:
            got = await hub.answer("laptop-1", one_candidate(await page.offer()), "192.168.1.20")
            connection = hub.peers["laptop-1"].pc.sctp.transport.transport.iceGatherer._connection
            assert connection.stun_server is None and connection.turn_server is None
            assert "typ srflx" not in got["sdp"] and "typ relay" not in got["sdp"]
            assert got["voice"] is False                                              # no calls on this hub
            # positive control: left to aiortc, the same gatherer would ask Google's STUN server
            default = aiortc.RTCIceGatherer(iceServers=None)._connection
            assert default.stun_server == ("stun.l.google.com", 19302)
            await asyncio.sleep(0.1)
        finally:
            await page.close()
            await hub.close()

    asyncio.run(go())


CANDIDATES = ("a=candidate:1 1 udp 2122260223 0a1b2c3d-1111-2222-3333-444455556666.local 54321 typ host generation 0"
              " network-id 1 network-cost 10\r\n"
              "a=candidate:2 1 udp 2122194687 F2E1D0C9-AAAA-BBBB-CCCC-DDDDEEEEFFFF.Local 50000 typ host generation 0\r\n"
              "a=candidate:3 1 udp 1686052607 0a1b2c3d-1111-2222-3333-444455556666.local 61000 typ srflx raddr 0.0.0.0"
              " rport 0\r\n"
              "a=candidate:4 1 udp 2122129151 192.168.1.50 54400 typ host\r\n")
SDP = ("v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n"
       + CANDIDATES)


@pytest.mark.parametrize("peer,expect", [
    ("192.168.1.23", "192.168.1.23"), ("10.0.0.7", "10.0.0.7"), ("::ffff:192.168.1.23", "192.168.1.23"),
    ("[::ffff:192.168.1.23]", "192.168.1.23"), ("169.254.3.4", "169.254.3.4"),
    ("127.0.0.1", None), ("::1", None), ("fe80::1%en0", None), ("2001:db8::5", None), ("fd00::5", None),
    ("0.0.0.0", None), ("224.0.0.251", None), ("not an address", None), ("", None)])
def test_mdns_host_candidates_point_at_an_ipv4_peer_only(peer: str, expect: Optional[str]) -> None:
    out, n = rewrite_local_candidates(SDP, peer)
    if expect is None:
        assert (out, n) == (SDP, 0)
        return
    assert n == 2
    lines = out.split("\r\n")
    assert f"a=candidate:1 1 udp 2122260223 {expect} 54321 typ host generation 0 network-id 1 network-cost 10" in lines
    assert f"a=candidate:2 1 udp 2122194687 {expect} 50000 typ host generation 0" in lines
    assert "0a1b2c3d-1111-2222-3333-444455556666.local 61000 typ srflx" in out             # only host candidates
    assert "a=candidate:4 1 udp 2122129151 192.168.1.50 54400 typ host" in lines          # an address stays
    assert out.count("\r\n") == SDP.count("\r\n") and out.replace(expect, "") != out
    lf, m = rewrite_local_candidates(SDP.replace("\r\n", "\n"), peer)                     # line endings are kept
    assert m == 2 and "\r" not in lf


def test_an_offer_hidden_behind_mdns_names_connects_through_the_rewrite(tmp_path: Path) -> None:
    async def go() -> None:
        link = door(tmp_path, FakeInlet(store_at(tmp_path)))
        hub = RtcHub(link, None)
        page = Page()
        try:
            offer = await page.offer()
            host = re.compile(r"^(a=candidate:\S+ \d+ udp \d+ )(\S+)( \d+ typ host.*)$")
            v4 = [m.group(2) for m in map(host.match, offer.split("\r\n")) if m and ":" not in m.group(2)]
            if not v4:
                pytest.skip("this Mac has no IPv4 address for the peers to meet on")
            # as a browser sends it: every IPv4 host candidate behind an mDNS name, and no IPv6 one to fall back on
            lines = []
            for line in offer.split("\r\n"):
                m = host.match(line)
                if m and ":" in m.group(2):
                    continue
                if line.startswith("c=IN IP4 "):
                    line = "c=IN IP4 0.0.0.0"
                lines.append(f"{m.group(1)}{uuid.uuid4()}.local{m.group(3)}" if m else line)
            hidden = "\r\n".join(lines)
            candidates = [ln for ln in hidden.split("\r\n") if ln.startswith("a=candidate:")]
            assert candidates and all(".local " in ln for ln in candidates)
            got = await hub.answer("phone-2", hidden, v4[0])
            remote = hub.peers["phone-2"].pc.remoteDescription.sdp
            seen = [ln for ln in remote.split("\r\n") if ln.startswith("a=candidate:")]
            assert ".local" not in remote and len(seen) == len(candidates)
            assert all(f" {v4[0]} " in ln for ln in seen)
            await page.accept(got["sdp"])
            await until(lambda: page.link.readyState == "open", 20)                   # it connected that way
            page.link.send(json.dumps(req(1, "GET", MESSAGES)))
            await until(lambda: answers(page.link_got).get(1, {}).get("end"), 10)
            assert answers(page.link_got)[1]["head"]["s"] == 200
        finally:
            await page.close()
            await hub.close()
            link.close()

    asyncio.run(go())


def test_one_peer_per_device_the_newest_offer_wins_and_at_most_four() -> None:
    async def go() -> None:
        hub = RtcHub(None, None)
        pages = [Page() for _ in range(6)]
        try:
            for i in range(5):
                await hub.answer(f"device-{i}", one_candidate(await pages[i].offer()), "192.168.1.30")
                await asyncio.sleep(0.1)
            assert list(hub.peers) == [f"device-{i}" for i in range(1, 5)]           # the oldest made room
            old = hub.peers["device-2"]
            await hub.answer("device-2", one_candidate(await pages[5].offer()), "192.168.1.30")   # the same device
            assert hub.peers["device-2"] is not old and old.closed
            assert list(hub.peers) == ["device-1", "device-3", "device-4", "device-2"]
            await until(lambda: old.pc is None)
            assert hub.stats()["answers"] == 6 and hub.stats()["peers"] == 4
            await asyncio.sleep(0.1)
            peers = list(hub.peers.values())
            late = Page()
            pages.append(late)
            offer = await late.offer()
            await hub.close()
            assert hub.peers == {} and all(p.closed and p.pc is None for p in peers)
            with pytest.raises(RtcError):
                await hub.answer("device-9", offer, "192.168.1.30")                  # a stopping hub takes none
        finally:
            for page in pages:
                await page.close()

    asyncio.run(go())


@pytest.mark.parametrize("sdp", ["", "   ", "x" * (lite_rtc.MAX_SDP_CHARS + 1), None, 5,
                                 "v=0\r\nnot really sdp\r\n", "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n"])
def test_what_is_not_an_offer_with_a_data_channel_is_refused(sdp: Any) -> None:
    async def go() -> None:
        hub = RtcHub(None, None)
        with pytest.raises(RtcError) as refused:
            await hub.answer("device-1", sdp, "192.168.1.2")
        if isinstance(sdp, str) and sdp.startswith("v=0"):
            assert str(refused.value) == "the offer has no data channel"            # aiortc took it; this did not
        assert hub.peers == {} and hub.stats()["answers"] == 0
        await hub.close()

    asyncio.run(go())


def test_without_aiortc_every_offer_is_refused_and_nothing_else_breaks(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setitem(sys.modules, "aiortc", None)                               # as if it did not load
    assert lite_rtc.available() is False

    async def go() -> None:
        hub = RtcHub(None, None)
        with pytest.raises(RtcError, match="not available"):
            await hub.answer("device-1", SDP, "192.168.1.2")
        assert hub.peers == {} and hub.stats()["answers"] == 0
        await hub.close()

    asyncio.run(go())


def test_a_device_id_is_needed_and_a_peer_that_never_connects_is_closed() -> None:
    async def go() -> None:
        hub = RtcHub(None, None, connect_secs=0.5)
        page = Page()
        try:
            offer = one_candidate(await page.offer())
            for bad in ("", None):
                with pytest.raises(RtcError):
                    await hub.answer(bad, offer, "192.168.1.2")                      # type: ignore[arg-type]
            await hub.answer("device-1", offer, "192.168.1.2")                       # the page never applies it
            peer = hub.peers["device-1"]
            await until(lambda: hub.peers == {} and peer.closed and peer.pc is None, 5)
        finally:
            await page.close()
            await hub.close()

    asyncio.run(go())


def test_revoking_a_device_ends_its_requests_and_its_call(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        link = door(tmp_path, inlet, poll_secs=0.05)
        brain = Brain()
        calls = PhoneCalls(lambda _: None, brain, Voice())
        hub = RtcHub(link, calls)
        page = Page()
        try:
            async with asyncio.timeout(40):
                got = await hub.answer("phone-3", await page.offer(), "127.0.0.1")
                await page.accept(got["sdp"])
                await until(lambda: page.link.readyState == "open", 20)
                page.link.send(json.dumps(req(1, "GET", MESSAGES)))
                await until(lambda: answers(page.link_got).get(1, {}).get("end"), 10)
                cursor = json.loads(answers(page.link_got)[1]["body"])["cursor"]
                page.link.send(json.dumps(req(2, "GET", MESSAGES, query=f"after={cursor}&wait=25")))   # a long poll
                page.open_call()
                await until(lambda: calls.active is not None, 10)
                peer = hub.peers["phone-3"]
                await until(lambda: peer.link is not None and 2 in peer.link.requests, 10)
                poll = peer.link.requests[2].task
                await hub.close_device("phone-3")
                assert hub.peers == {} and peer.closed
                await until(lambda: calls.active is None and poll is not None and poll.done(), 10)
                assert 2 not in answers(page.link_got)                                # never answered: cancelled
                await hub.close_device("phone-3")                                     # twice: nothing to close
        finally:
            await page.close()
            await hub.close()
            link.close()

    asyncio.run(go())


# ---- the channels a peer takes ----------------------------------------------------------------------------------

def test_a_peer_takes_one_link_and_one_call_channel_and_refuses_the_rest(tmp_path: Path) -> None:
    async def go() -> None:
        link = door(tmp_path, FakeInlet(store_at(tmp_path)))
        calls = PhoneCalls(lambda _: None, Brain(), Voice())
        hub = RtcHub(link, calls)
        peer = lite_rtc._Peer(hub, "device-1")
        for refused in (FakeChannel("video"), FakeChannel("link", ordered=False), FakeChannel("")):
            peer._on_channel(refused)
            assert refused.readyState == "closed" and peer.link is None
        lossy = FakeChannel("call")
        lossy.maxRetransmits = 0
        peer._on_channel(lossy)
        assert lossy.readyState == "closed" and peer.call is None
        first, second = FakeChannel("link"), FakeChannel("link")
        peer._on_channel(first)
        peer._on_channel(second)                                                     # the newest wins
        assert peer.link is not None and peer.link.channel is second and first.readyState == "closed"
        # a connection whose offer had no audio cannot take a call: the page is told why
        no_audio = FakeChannel("call")
        peer._on_channel(no_audio)
        assert no_audio.readyState == "closed" and peer.call is None
        assert [json.loads(m) for m in no_audio.sent] == [{"type": "ended", "reason": lite_rtc.NO_AUDIO}]
        # with voice on, the call channel goes to PhoneCalls with the peer's secret as its hello's token
        peer.voice_on = True
        call = FakeChannel("call")
        peer._on_channel(call)
        await until(lambda: {"type": "state", "state": "connected"} in [json.loads(m) for m in call.sent], 5)
        assert calls.active is not None and peer.call is not None
        call2 = FakeChannel("call")
        peer._on_channel(call2)                                                      # a second call on this peer
        assert call.readyState == "closed"
        await until(lambda: {"type": "state", "state": "connected"} in [json.loads(m) for m in call2.sent], 5)
        await peer.close()
        assert call2.readyState == "closed" and second.readyState == "closed"
        await until(lambda: calls.active is None, 5)
        link.close()

    asyncio.run(go())


# ---- under eve lite's process firewall ---------------------------------------------------------------------------

SESSION = """
import asyncio, json, subprocess, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from eve_bridge import profile
profile.install_firewall("enforce")
import test_lite_rtc_loopback as t
from test_link_chat import FakeInlet, store_at, two_lines
from test_lite_link_channel import answers, door, req, until
from test_phone_call import Brain
from eve_bridge.lite_rtc import RtcHub
from eve_bridge.phone_call import PhoneCalls

async def go(tmp):
    inlet = FakeInlet(store_at(tmp), two_lines)
    link = door(tmp, inlet)
    voice = t.ToneVoice()
    hub = RtcHub(link, PhoneCalls(lambda _: None, Brain(reply="Hi."), voice))
    page = t.Page()
    try:
        got = await hub.answer("device-1", await page.offer(), "127.0.0.1")
        await page.accept(got["sdp"])
        await until(lambda: page.link.readyState == "open", 20)
        page.link.send(json.dumps(req(1, "POST", "/api/link/turn", {"text": "hi", "client_id": "firewall-1"})))
        await until(lambda: answers(page.link_got).get(1, {}).get("end"), 10)
        page.audio.sender.replaceTrack(t.Tone(1000.0))
        page.open_call()
        await until(lambda: page.call.readyState == "open", 10)
        await asyncio.sleep(0.4)
        page.say({"type": "talk"})
        await asyncio.sleep(0.6)
        page.say({"type": "done"})
        await until(lambda: voice.pcm is not None, 10)
        await until(lambda: max(page.heard, default=0.0) > 2000, 10)
        page.say({"type": "end"})
        await asyncio.sleep(0.2)
    finally:
        await page.close()
        await hub.close()
        link.close()

asyncio.run(go(Path(sys.argv[2])))
during = profile.firewall_counts()
try:
    subprocess.run(["/usr/bin/true"])
    ran = True
except PermissionError:
    ran = False
print("RESULT " + json.dumps({"during": during, "control_ran": ran, "after": profile.firewall_counts()}))
"""


def test_a_webrtc_session_under_the_lite_firewall_starts_no_program_and_opens_no_protected_folder(
        tmp_path: Path) -> None:
    """The process firewall eve lite runs under (profile.install_firewall) refuses every program but three and the
    private folders. One whole session, a turn over link and a call with eve's spoken answer, runs under it in a
    subprocess (an audit hook cannot be removed) and is refused nothing; then a program start is (the control)."""
    profile = pytest.importorskip("eve_bridge.profile")
    if not hasattr(profile, "install_firewall") or not hasattr(profile, "firewall_counts"):
        pytest.skip("profile.py has no process firewall yet")
    home, work = tmp_path / "home", tmp_path / "work"
    home.mkdir()
    work.mkdir()
    env = {**os.environ, "EVE_PROFILE": "lite", "HOME": str(home)}
    done = subprocess.run([sys.executable, "-c", SESSION, str(Path(__file__).parent), str(work)], env=env,
                          capture_output=True, text=True, timeout=180)
    line = next((ln for ln in done.stdout.splitlines() if ln.startswith("RESULT ")), None)
    assert done.returncode == 0 and line is not None, done.stderr[-3000:]
    result = json.loads(line[len("RESULT "):])
    assert result["during"] == {}                                                    # nothing refused, nothing tried
    assert result["control_ran"] is False and result["after"] == {"exec:true": 1}  # and the firewall was on
