"""Main daemon: wires IPC, BLE, state, and JSONL tailer together."""

from __future__ import annotations

import asyncio
import logging
import os
import threading
import time
from datetime import datetime
from typing import TYPE_CHECKING, Any, Optional

if TYPE_CHECKING:
    from .key_tap import KeyTapper

from . import app_registry, claude_live, mac_camera, mem0_memory, openrouter, photos, voice_agent
from . import dream as dream_mod
from . import follow as follow_mod
from . import memory as memory_mod
from . import recall as recall_mod
from . import records as records_mod
from . import telegram as telegram_mod
from . import transcripts as transcripts_mod
from .audit import AuditLog
from .ble import EveBLE
from .caption_pager import CaptionPager, PagerConfig
from .codex_computer import CodexComputerAgent
from .computer_agent import configured as agent_configured
from .diary import DiaryTaker, Emote, Thought, build_emote_cmd, make_diary_client
from .ears import Ears, keyword_id
from .ears import configured as ears_configured
from .explore import (
    Action,
    Explorer,
    ExploreRefused,
    Look,
    Mode,
    Note,
    NoteTaker,
    Rest,
    append_note,
    build_look_cmd,
    build_mode_cmd,
)
from .explore import configured as explore_configured
from .head import Head
from .identity import FaceIdentity, OwnerIdentity, configured_threshold, make_describer
from .identity import default_path as identity_default_path
from .intent import make_intent_classifier
from .ipc import IPCServer
from .jsonl_tailer import JSONLTailer
from .learning.think_aloud import LISTEN_ACTIONS
from .listen_key import Stopper, start_listen_key
from .live_expressions import LiveExpressions, phone_expressions_on
from .matchers import MatcherConfig, classify_command
from .matchers import load_config as load_matcher_config
from .memory_bus import MemoryBus
from .memory_bus import configured as bus_configured
from .notes import RoomNotes, make_notes_client
from .notes import configured as notes_configured
from .protocol import (
    ENTRY_MAX_BYTES,
    HEARTBEAT_KEEPALIVE,
    build_heartbeat,
    build_time_sync,
    truncate_utf8_bytes,
)
from .read_policy import is_within
from .scene import SceneWatcher, make_scene_client
from .scene import configured as scene_configured
from .sound import SoundSetting, build_sound_cmd, quiet_caption
from .state import State, notification_waits
from .think import configured as think_configured
from .think import make_thinker
from .thought_screen import ThoughtScreen
from .version_check import check as version_check
from .vision import STATS_INTERVAL_SECS as VISION_STATS_SECS
from .vision import (
    FaceTracker,
    Frame,
    build_cam_cmd,
    build_snap_cmd,
    configured_save_dir,
    decode_frame,
    make_detector,
)

# Entry text is prefixed with a 2-byte marker ("> ", "@ ", "+ ") before being
# stored. Budget the user-supplied portion so the full entry stays within the
# firmware's line buffer without _format_entry needing to re-truncate.
_ENTRY_PAYLOAD_MAX_BYTES = ENTRY_MAX_BYTES - 2

log = logging.getLogger(__name__)

# Think out loud: how long the learning server's thread waits for the loop to take a listen request,
# and the name of a conversation task opened for listening (so a stop can cancel one still connecting).
THINK_ALOUD_CALL_SECS = 12.0
THINK_ALOUD_TASK = "think-aloud-conversation"
# Memory (owner, 2026-09-23). A voice conversation opens with today's lines only when they are built this fast;
# shutdown gives the close markers and then the background work (the dream) this long.
TODAY_BUILD_SECS = 0.05
CLOSE_MARKERS_SECS = 3.0
DRAIN_SECS = 12.0

class Daemon:
    def __init__(
        self,
        socket_path: Optional[str] = None,
        device_name_prefix: str = "Claude",
        device_address: Optional[str] = None,
        matchers: Optional[MatcherConfig] = None,
        serial_port: Optional[str] = None,
        save_frames: Optional[str] = None,
    ) -> None:
        self.state = State()
        self.ipc = IPCServer(self._handle_ipc, socket_path=socket_path) if socket_path else IPCServer(self._handle_ipc)
        if serial_port:
            # USB CDC transport (Freenove FNK0104B port): same wire protocol,
            # different pipe. Kept on the .ble attribute so the rest of the
            # daemon doesn't care which transport is live.
            from .controller import Controller, controller_serial
            from .serial_transport import EveSerial, serial_skip
            # A controller board (controller.py, the Voice PE) is named by its
            # USB serial; the robot's glob must never open it, nor any board in
            # EVE_SERIAL_SKIP.
            ctl_serial = controller_serial()
            skip = serial_skip() | (frozenset({ctl_serial.lower()}) if ctl_serial else frozenset())
            self.ble = EveSerial(on_message=self._handle_ble, port=serial_port, skip_serials=skip)
            # A board reboot under an unbroken CH340 link never fires the
            # on-connect resync — replay it when the boot banner scrolls past,
            # or the reborn board keeps "--:--" and "No Claude" indefinitely.
            self.ble.on_boot = self._resync_board
            self._controller: Optional[Controller] = None
            if ctl_serial:
                self._controller = Controller(ctl_serial, self._handle_ble)
                robot_send = self.ble.send

                async def send_both(obj: dict[str, Any], codec: Optional[str] = None) -> bool:
                    # Every send to the robot, tee'd: the controller keeps the
                    # heartbeat, time, conversation state and sound (controller.mirrored).
                    ok = await robot_send(obj, codec)
                    await self._controller.mirror(obj)
                    return ok

                self.ble.send = send_both  # type: ignore[method-assign]
        else:
            self.ble = EveBLE(
                on_message=self._handle_ble,
                name_prefix=device_name_prefix,
                address=device_address,
            )
        self.jsonl = JSONLTailer(self._on_tokens, on_assistant_text=self._on_assistant_text)
        self.matchers = matchers if matchers is not None else load_matcher_config()
        # Per-decision append-only log; see audit.py
        self.audit = AuditLog()
        # Hold-the-pet push-to-talk: holds Opt+Space (VoiceFlow) between the
        # stick's voice start/stop events. Lazily constructed on first use so
        # non-mac / Quartz-less hosts pay nothing.
        self._keys: Optional["KeyTapper"] = None
        # Listen key (held Option = dictation): a Quartz tap on its own
        # thread, started in run(). None when the tap could not be created.
        self._listen_stop: Optional[Stopper] = None
        # Last {"cmd":"listen"} state the board received. None until the
        # first send; reset to False on every connect/resync so a rebooted
        # board never keeps the listening pose.
        self._listen_sent: Optional[bool] = None
        # Host vision: the board streams camera frames, we answer with the
        # face position. The detector (macOS Vision) is resolved in run() so
        # constructing a Daemon never imports pyobjc; until then, and on
        # hosts without Vision, the tracker is disabled and we never ask the
        # board to stream — it keeps its on-board tracking.
        self._vision = FaceTracker(
            detect=None, send=self.ble.send, save_dir=configured_save_dir(save_frames))
        # Physical listen-key state, independent of what the board received:
        # identity.py enrols the owner only while it is down. Written on the
        # loop thread, read on the vision executor thread (a bool: no lock).
        self._listen_down = False
        self._ptt_held = False          # the board's hold-to-talk button, by its last ptt edge
        # Owner identity: the prints live in memory; the file is loaded and
        # the Vision describer resolved in run(), like the detector.
        self._identity: Optional[FaceIdentity] = None
        # Eyes for the voice (scene.py): camera frames become timestamped
        # descriptions, only while a conversation is open. The client (an
        # image model) is resolved in run(), like the detector.
        self._scene = SceneWatcher(None, scene_configured(),
                                   camera_ok=lambda: self.ble.connected and self._vision.enabled)
        # Head control for the voice (head.py). The board echoes its real
        # pose on every camera frame; relative moves start from that.
        self._head = Head(self.ble.send, connected=lambda: self.ble.connected)
        # In a conversation eve keeps its eyes on whoever is talking (follow.py): every face of every
        # frame goes to the follower, which stands down for an asked-for pose, an explore, the dictation
        # key and the lesson's listening pose.
        self._follower = follow_mod.SpeakerFollower(
            self.ble.send, enabled=follow_mod.configured(),
            presence=follow_mod.PresenceMap(follow_mod.presence_path()).load(),
            publish=lambda topic, msg: self.bus.publish(topic, msg),      # /eve/presence: seen, lost, found and how
            blocked=lambda: ("explore" if self._explorer.active else "dictation" if self._listen_down
                             else "lesson listening pose" if self._think_aloud_state == "on" else ""))
        self._head.on_move = self._follower.owner_moved
        self._vision.on_faces = self._follower.on_faces
        # The owner's mute choice (sound.py): persisted, re-sent on every connect.
        self._sound = SoundSetting()
        self._sound.load()
        self._expressions = LiveExpressions(self.ble.send, connected=lambda: self.ble.connected,
                                           muted=lambda: self._sound.muted,
                                           phase=lambda: getattr(self, "_agent_state", "idle"))
        self._expression_audition = None
        self._phone_expressions = None     # the phone's picker, made with the Mini App (_start_miniapp)
        # The owner's microphone switch (`eve-bridge mic`, or the menu-bar
        # app): persisted, and one of the two things the mic policy checks.
        self._mic = SoundSetting(name="mic")
        self._mic.load()
        # "Go away" / "mute" in any words (intent.py); resolved in run().
        self._intent: Optional[Any] = None
        self._thinker: Optional[Any] = None
        # Idle explorer (explore.py): after a quiet stretch the board pans
        # the room and, per waypoint, may spend a note on what it sees. The
        # pure Explorer is ticked from _explore_loop; the note client is
        # resolved in run() so constructing a Daemon never imports openai.
        self._explore_cfg = explore_configured()
        self._explorer = Explorer(self._explore_cfg, now=time.monotonic())
        self._notes: Optional[NoteTaker] = None
        # "hey eve": the wake word on the Mac mic (ears.py) opens a spoken
        # conversation (voice_agent.py) that can run a computer-use task
        # (computer_agent.py). One conversation at a time; the board mirrors
        # its phases through {"cmd":"agent","state":...}.
        self._ears_cfg = ears_configured()
        self._voice_cfg = voice_agent.configured()
        self._learning_server = None
        # The pending "back to idle" after a lesson caption, and a count of lesson notices so
        # _handle_lesson can tell whether dispatch already put a caption on the robot.
        self._learning_idle_handle: Optional[asyncio.TimerHandle] = None
        self._learning_notices = 0
        # Think out loud (learning/think_aloud.py). All of it changes on the loop thread only.
        #   _voice_session      the open conversation's VoiceSession, once connected
        #   _think_aloud_wish   a lesson waiting for a conversation that is still connecting or closing
        #   _think_aloud_state  "off", "starting" (asked, microphone not yet live) or "on"
        self._voice_session: Optional[voice_agent.VoiceSession] = None
        self._think_aloud_wish: Optional[dict[str, Any]] = None
        self._think_aloud_state = "off"
        self._think_aloud_lesson_id: Optional[str] = None
        self._recall_cfg = recall_mod.configured()
        # eve's memory as it forms (memory_bus.py): every kept diary thought, lesson event
        # and state change is published the moment it happens. What was SAID never goes on
        # the bus: it stays in the memory folder (owner, 2026-09-23). Sinks (a rosbridge
        # WebSocket server, the owner's claude-mem) attach in run() behind env flags, all off
        # by default. Bound to the loop in run().
        self.bus = MemoryBus()
        self._bus_cfg = bus_configured()
        self._rosbridge: Optional[Any] = None
        self._claude_mem_sink: Optional[Any] = None
        self._claude_mem_mirror: Optional[Any] = None
        # eve's memory of what was SAID (memory.py): one facade over the transcripts, the records and
        # the mem0 index, opened at the top of run() once the one-time move to the memory folder is done.
        # None while memory is off (EVE_MEMORY): then nothing is written and every prompt is as before.
        # Separate from the diary, which remembers what it SAW.
        self._memory: Optional[memory_mod.Memory] = None
        # The newest voice conversation's transcript id ("" when none): shutdown closes it if its session did not.
        self._voice_conv = ""
        # Fire-and-forget work that must outlive the call that started it, held so
        # it is not garbage-collected mid-flight.
        self._background: set[asyncio.Task[Any]] = set()
        # The last few seconds of reported head poses, for measuring a motion.
        self._motion_trace: list[tuple[float, float, float]] = []
        # Recording the room on request (notes.py), built lazily so a daemon that
        # is never asked never touches the microphone for it. NOT self._notes:
        # that is the diary's vision note taker, and clobbering it would cost
        # eve its eyes.
        self._room_notes: Optional[RoomNotes] = None
        self._agent_cfg = agent_configured()
        self._ears: Optional[Ears] = None
        self._conversation: Optional[asyncio.Task[None]] = None
        self._agent_state = "idle"
        self._active_agent: Optional[CodexComputerAgent] = None
        # The text door (telegram.py). None unless EVE_TELEGRAM is on with a token and an owner id.
        self._telegram: Optional[telegram_mod.TelegramInlet] = None
        # "hey eve, go explore": the voice tool sets this; the explore
        # starts when the conversation has closed, so the board is never
        # asked to hold a conversation pose and pan the room at once.
        self._explore_after_conversation: Optional[str] = None
        # Thoughts on the robot's own screen while it explores: the same pager
        # the conversation uses (caption_pager.py), so a thought is wrapped to
        # 4 lines x 17 chars and held at reading pace. A conversation owns the
        # screen when one is open; this only ever draws between them.
        self._thought_pager = CaptionPager(PagerConfig(read_cps=self._voice_cfg.caption_cps))
        # ... and what is worth putting there. The diary writes down far more
        # than a person sitting beside the robot wants to read.
        self._screen = ThoughtScreen()
        # What the room sounds like (hearing.py). The board reports a loudness
        # reading while it explores; a frame cannot tell a silent afternoon
        # from one where a door just banged.
        # Monotonic time of the last thing a human or a session did: any hook
        # event, a board touch, the listen key. Idle time is measured from it.
        self._last_activity_at = time.monotonic()
        # The newest raw {"frame":...} object, kept only while exploring so a
        # waypoint sample never costs a decode on the 5 fps path.
        self._explore_raw_frame: Optional[dict[str, Any]] = None
        # A pending {"cmd":"snap"}: the diary asked for a photo and waits on
        # this future for the one frame line the board answers with.
        self._snap_waiter: Optional[asyncio.Future[Frame]] = None
        # Most recent {"diag":{...}} the board sent; served over IPC so
        # `eve-bridge diag` can show it without owning the serial port.
        self._last_diag: Optional[dict[str, Any]] = None
        # Status-ack liveness: proves host->board writes still land.
        self._status_sent_at: Optional[float] = None
        self._status_missed = 0
        self._ack_escalation = 0   # consecutive missed-ack episodes
        self._clean_polls = 0      # consecutive answered watchdog polls (resync acks excluded)
        # session_id → task that'll flip running→0 after a grace window.
        # Delays the turn_end so the stick's HUD stays drawn long enough to
        # display the @-entry the tailer just emitted. See firmware's
        # drawHUD/clocking gate in main.cpp.
        self._pending_turn_ends: dict[str, asyncio.Task] = {}
        # Cached stick-side status fields from the most recent status ack.
        self._last_stick_sec: Optional[bool] = None
        self._last_stick_battery_pct: Optional[int] = None
        # Futures awaiting a specific ack type. Used by folder_push's
        # chunk-by-chunk flow control. Each entry: (ack_type, n, Future); n,
        # when not None, is the ack's "n" the waiter expects (see expect_ack).
        self._ack_waiters: list[tuple[str, Optional[int], asyncio.Future]] = []
        # Track last heartbeat to dedupe (avoid spamming BLE with identical snapshots).
        self._last_hb_serialized: Optional[str] = None
        self._last_hb_sent_at: float = 0.0
        # Latest available version string (e.g. "v0.1.1") when an update is
        # available; None otherwise. Set by _update_check_loop.
        self._update_available: Optional[str] = None
        # Wire codec for heartbeat string fields. Set when the user has
        # flashed the fork-only CJK firmware variant (`m5stickc-plus-cjk-*`)
        # and tells the bridge which one via env var. None = stock firmware,
        # ASCII-only content. See protocol.CJK_CODECS for the mapping.
        from .protocol import CJK_CODECS
        _target = os.environ.get("EVE_CJK_TARGET", "").strip()
        self._cjk_codec: Optional[str] = CJK_CODECS.get(_target) if _target else None
        if self._cjk_codec is not None:
            log.info("cjk firmware target=%s, wire codec=%s", _target, self._cjk_codec)
        self._shutdown = asyncio.Event()

    # ---- entry ----

    async def run(self) -> None:
        _log_permission_config_summary(self.matchers)
        # Memory first: the one-time move into the memory folder must run before anything opens a store,
        # and the IPC server below can already write meeting notes into it.
        await Daemon._open_memory(self)
        # The voice SDK's resources package imports in 12–20 s inside this process
        # (GIL contention with vision and the keyword spotter). Pay it now, off the
        # loop, so the first "hey eve" does not (voice_agent.warm_live_import).
        threading.Thread(target=voice_agent.warm_live_import, name="warm-voice-sdk", daemon=True).start()
        await self.ipc.start()
        if os.environ.get("EVE_LEARNING", "1").strip().lower() not in ("0", "false", "no", "off"):
            from .learning.server import start as start_learning
            loop = asyncio.get_running_loop()
            def learning_notice(text, stage):
                loop.call_soon_threadsafe(self._learning_notice, text, stage)
            try:
                self._learning_server = start_learning(
                    port=int(os.environ.get("EVE_LEARNING_PORT", "48766")), notify=learning_notice,
                    listener=self._make_listener(loop))
                log.info("learning: %s", self._learning_server.app.url)
            except (OSError, ValueError):
                log.exception("learning: could not start the local workspace")
        self.bus.bind_loop(asyncio.get_running_loop())
        await self._start_memory_sinks()
        self._listen_stop = start_listen_key(self._on_listen_key, asyncio.get_running_loop())
        self._start_ears(asyncio.get_running_loop())
        self._vision.detect = make_detector()
        self._identity = self._make_identity()
        self._vision.identity = self._identity
        self._scene.client = make_scene_client(self._scene.config)
        self._intent = make_intent_classifier()
        # The slow brain behind the voice (think.py): high-effort reasoning with
        # web search, for the backend's think_hard tool.
        self._thinker = make_thinker(think_configured(backend_model=self._voice_cfg.backend_model))
        if self._sound.muted:
            log.info("sound: muted (owner's choice, %s) — the head and lights still move", self._sound.path)
        tasks = [
            asyncio.create_task(self._expressions.run(), name="laya-expressions"),
            asyncio.create_task(self.ipc.serve_forever(), name="ipc"),
            asyncio.create_task(self.ble.run(), name="ble"),
            *([asyncio.create_task(self._controller.run(), name="controller")]
              if getattr(self, "_controller", None) is not None else []),
            asyncio.create_task(self.jsonl.run(), name="jsonl"),
            asyncio.create_task(self._heartbeat_loop(), name="heartbeat"),
            asyncio.create_task(self._on_ble_connected(), name="on-connect"),
            asyncio.create_task(self._status_poller(), name="status-poller"),
            asyncio.create_task(self._update_check_loop(), name="update-check"),
            asyncio.create_task(self._vision_stats_loop(), name="vision-stats"),
        ]
        # The diary (diary.py): memory-aware thoughts plus an appraisal
        # the board turns into a feeling. Same take() shape as NoteTaker.
        # The loop always runs so `eve-bridge explore` and "hey eve,
        # go explore" work; EVE_EXPLORE=0 only turns off the idle start.
        client = make_diary_client(self._explore_cfg.model)
        self._explorer.notes_enabled = client is not None
        if client is not None:
            self._notes = DiaryTaker(client, self._explore_cfg.notes_dir, send_emote=self._send_emote,
                                     snapshot=self._take_snapshot, on_thought=self._show_thought,
                                     on_written=lambda rec: Daemon._publish_observation(self, rec))
        tasks.append(asyncio.create_task(self._explore_loop(), name="explore"))
        tasks.append(asyncio.create_task(self._thought_caption_loop(), name="thought-captions"))
        # codex_warm.py: one Codex agent started ahead of time, so a hard task's handoff is the turn only.
        from . import codex_warm
        warm_on, warm_age = codex_warm.configured()
        Daemon._warn_removed_computer()
        self._codex_warm = codex_warm.WarmCodex(CodexComputerAgent, enabled=warm_on and self._agent_cfg.enabled,
                                                max_age=warm_age)
        tasks.append(asyncio.create_task(self._codex_warm.refresh_loop(), name="codex-warm"))
        # The fast path into the owner's logged-in Chrome (chrome_lane.py): ONE attached lane for the daemon's
        # life, so Chrome's "Allow remote debugging?" is answered once per Chrome session (over Telegram).
        self._chrome_lane = Daemon._make_chrome_lane(self)
        self._telegram = self._make_telegram()
        if self._telegram is not None:
            tasks.append(asyncio.create_task(self._telegram.run(), name="telegram"))
            self._command_risk()                     # one log line at start: the Auto Mode gate's mode
            # The watcher (watch.py): prices, quotes and ticket releases on a schedule, texted through the door.
            watcher = getattr(self, "_watcher", None)
            if watcher is not None:
                watcher.notify = self._telegram.tell_owner
                tasks.append(asyncio.create_task(watcher.run(), name="watch"))
            # The chief of staff (chief.py), when EVE_CHIEF has it on: its loop, beside the watcher's. It sends
            # through the door's own tell_owner (chief_notify), built with it in _make_telegram.
            chief = getattr(self._telegram, "chief", None)
            if chief is not None:
                tasks.append(asyncio.create_task(chief.run(), name="chief"))
            # The Meet notetaker (meet.py) texts through the same door: in the call, the lobby, the notes.
            meeter = getattr(self, "_meeter", None)
            if meeter is not None:
                inlet = self._telegram

                async def meet_notify(text: str, about: str = "") -> None:
                    await inlet.tell_owner(text, about=about, title=telegram_mod.MEET_TITLE)

                meeter.notify = meet_notify
        # The "Ask Claude" Mini App (miniapp.py): the owner's menu button opens a Claude chat inside Telegram,
        # served here through a Cloudflare quick tunnel. Off unless EVE_MINIAPP=1.
        Daemon._start_miniapp(self, tasks)
        # Updates (updates.py, docs/updates.md): the Eve Link pane's cards, rebuilt in the background every
        # EVE_UPDATES_EVERY_MIN in day time and once at start when stale. Only while the app's door is up; EVE_UPDATES=0
        # turns it off.
        link = getattr(getattr(getattr(self, "_miniapp", None), "server", None), "link", None)
        if link is not None and getattr(link, "updates", None) is not None and link.updates.enabled:
            tasks.append(asyncio.create_task(link.updates.loop(self._shutdown), name="updates"))
        # The providers' own spend figures (spend_sync.py): OpenRouter always, OpenAI and Anthropic with an admin
        # key; at start and hourly, on a worker thread. The Spending view shows them beside eve's own meter.
        from . import spend_sync
        tasks.append(asyncio.create_task(spend_sync.loop(self._shutdown), name="spend-sync"))
        # The places history (places.py): hourly, raw fixes and visits past their days go, and home and work are
        # inferred again. Nothing runs while EVE_PLACES is off or nothing has been kept.
        from . import places
        tasks.append(asyncio.create_task(places.loop(self._shutdown), name="places"))
        # The nightly dream (dream.py): the one automatic writer of the records and the mem0 index. It
        # watches the shutdown event itself and is drained, not cancelled, at the end: a night half written
        # is worse than a restart a few seconds late.
        Daemon._start_dream(self)
        if not self._explore_cfg.enabled:
            log.info("explore: eve explores only when asked (`eve-bridge explore`, \"go explore\", a text); "
                     "EVE_EXPLORE=1 turns the idle start on")
        try:
            await self._shutdown.wait()
        finally:
            # Withdraw purchase consent before any slow device/service teardown
            # yields. A confirmation already sent still journals its outcome.
            if getattr(self, "_agentcard", None) is not None:
                inlet = getattr(self, "_telegram", None)
                purchases = getattr(inlet, "_purchases", None)
                if purchases is not None:
                    purchases.shutdown()
                self._agentcard.close()
            # Leave explore mode and stop the camera before the transport
            # task is cancelled — its reader owns the port close, so a send
            # after that goes nowhere.
            await self._stop_explore("daemon stopping")
            if self._expression_audition is not None:
                self._expression_audition.cancel()
                await asyncio.gather(self._expression_audition, return_exceptions=True)
            await self._send_cam(False)
            self._vision.stop()
            if getattr(self, "_meeter", None) is not None:
                await self._meeter.close()                  # a call in progress: its notes are written
            await Daemon._close_notes(self)                 # notes in progress, the phone's and the room's
            if getattr(self, "_music_mode", None) is not None:
                await self._music_mode.close()
            for app in app_registry.APPS:                   # eve's apps that were made: a held Bluetooth light goes
                hub = getattr(self, "_" + app.slot, None)   # back to the phone app, the car stops, the camera's
                if hub is not None:                         # picture ends (re-entrant: the Mini App closes some too)
                    await hub.close()
            if getattr(self, "_canvas", None) is not None:
                await self._canvas.close()
            if getattr(self, "_chrome_lane", None) is not None:
                await self._chrome_lane.close()             # eve's tab only; the owner's Chrome stays
            for t in tasks:
                t.cancel()
            for pend in list(self._pending_turn_ends.values()):
                if not pend.done():
                    pend.cancel()
            await asyncio.gather(*tasks, *self._pending_turn_ends.values(),
                                 return_exceptions=True)
            await self._cancel_active_task("the daemon is restarting")
            if self._conversation is not None and not self._conversation.done():
                self._conversation.cancel()
                await asyncio.gather(self._conversation, return_exceptions=True)
            if self._ears is not None:
                self._ears.stop()
            if self._listen_stop is not None:
                self._listen_stop()
            await Daemon._close_open_conversations(self)
            await Daemon._drain_background(self)
            await self.ble.stop()
            await self.ipc.stop()
            await self._stop_memory_sinks()
            if self._learning_server is not None:
                await asyncio.to_thread(self._learning_server.shutdown)
                self._learning_server.server_close()
                self._learning_server = None

    async def shutdown(self) -> None:
        self._shutdown.set()

    async def _close_notes(self) -> None:
        """Notes still recording when the daemon stops are written now, transcript only, so a restart never
        loses them: they live only in memory until written (review, 2026-10-04, of notes on the phone: "notes
        never lost"). Each gets ``phone_call.NOTES_SHUTDOWN_SECS``; the phone's are texted as usual."""
        from .phone_call import NOTES_SHUTDOWN_SECS

        server = getattr(getattr(self, "_miniapp", None), "server", None)
        phone = getattr(getattr(server, "calls", None), "notes", None)
        if phone is not None and getattr(phone, "active", False):
            try:
                await phone.stop("the daemon stopped", quick=True)
            except Exception as e:  # noqa: BLE001 — shutting down: best effort
                log.warning("notes: the phone's notes were not written at shutdown (%s)", type(e).__name__)
        room = getattr(self, "_room_notes", None)
        if room is not None and room.active:
            room.quick = True
            room.request_stop("the daemon stopped")
            if room.task is not None:
                try:
                    await asyncio.wait_for(asyncio.shield(room.task), timeout=NOTES_SHUTDOWN_SECS)
                except (asyncio.TimeoutError, asyncio.CancelledError):
                    log.warning("notes: not written within %.0f s of shutdown", NOTES_SHUTDOWN_SECS)
                except Exception as e:  # noqa: BLE001
                    log.warning("notes: not written at shutdown (%s)", type(e).__name__)

    # ---- memory: the one facade, the dream, and closing up ----

    def _build_memory(self) -> Optional["memory_mod.Memory"]:
        """The memory stack, or None while memory is off. Synchronous (renames and a few git calls): run()
        calls it on a worker thread.

        The move comes first and only with memory on (owner, 2026-09-23): with memory off nothing under
        ~/.config/eve-bridge is touched. After the move the records folder is a repository of its own
        again (the old one went to the archive), sealed so it can never be pushed."""
        if not memory_mod.enabled():
            log.info("memory: off (EVE_MEMORY) — nothing said is kept, and prompts carry no memory")
            return None
        cfg = self._recall_cfg
        try:
            memory_mod.migrate(cfg, recall_mod.legacy_store(), recall_mod.legacy_mem0())
        except Exception as e:  # noqa: BLE001 - a failed move leaves the old store where it was
            log.warning("memory: the move to the memory folder failed (%s) — the old store is untouched",
                        type(e).__name__)
        records_mod.ensure_repo(cfg.records_dir)
        transcripts = transcripts_mod.Transcripts(transcripts_mod.configured(), archive=cfg.archive_dir)
        index = mem0_memory.shared(cfg)
        log.info("memory: on — transcripts, records%s", ", mem0 index" if index is not None else " (no mem0 index)")
        if index is not None and mem0_memory.embedder_of() == "gemma":
            # the local embedder loads in ~3 s; loaded now, the first search is not the one that waits
            threading.Thread(target=index.warm, name="mem0-warm", daemon=True).start()
        return memory_mod.Memory(cfg, transcripts, index, background=True)   # only the daemon runs the index worker

    async def _open_memory(self) -> None:
        try:
            self._memory = await asyncio.to_thread(Daemon._build_memory, self)
        except Exception as e:  # noqa: BLE001 - memory is a nicety: the daemon runs without it
            log.warning("memory: could not open (%s) — running without memory", type(e).__name__)
            self._memory = None

    def _start_dream(self) -> Optional[asyncio.Task[Any]]:
        """The nightly dream, held in `_background` so shutdown drains it instead of cancelling it.
        No memory or no model client (records.make_client logs why): no dream."""
        memory = getattr(self, "_memory", None)
        if memory is None:
            return None
        client = records_mod.make_client()
        if client is None:
            return None
        dreamer = dream_mod.Dreamer(self._recall_cfg, memory.transcripts, client, memory.index)
        task = asyncio.create_task(dreamer.loop(self._shutdown), name="memory-dream")
        self._background.add(task)
        task.add_done_callback(self._background.discard)
        return task

    async def _close_open_conversations(self) -> None:
        """A restart must not leave a conversation open in the transcripts: the voice session and the text
        inlet write their own close markers on the way out, and this is the backstop for any that did not
        (a task cancelled before it ran). A conversation that wrote nothing gets nothing."""
        memory = getattr(self, "_memory", None)
        if memory is None:
            return
        inlet = getattr(self, "_telegram", None)
        open_convs = [("voice", getattr(self, "_voice_conv", "")),
                      ("telegram", str(getattr(inlet, "_conv", None) or "") if inlet is not None else "")]
        store = memory.transcripts

        def close_all() -> int:
            closed = 0
            for ch, conv in open_convs:
                if not conv:
                    continue
                lines = store.conv_lines(conv)
                if lines and not any(ln.get("kind") == "close" for ln in lines):
                    closed += bool(store.close(ch, conv))
            return closed

        try:
            closed = await asyncio.wait_for(asyncio.to_thread(close_all), timeout=CLOSE_MARKERS_SECS)
        except Exception as e:  # noqa: BLE001 - shutting down: best effort
            log.warning("memory: close markers not written at shutdown (%s)", type(e).__name__)
            return
        if inlet is not None and getattr(inlet, "_conv", None) is not None:
            inlet._conv = None                   # closed above: a late _close_chat must not close it twice
        self._voice_conv = ""
        if closed:
            log.info("memory: %d open conversation(s) closed at shutdown", closed)

    async def _drain_background(self) -> None:
        """Wait up to DRAIN_SECS for background work (the nightly dream) to finish, then cancel
        whatever is left."""
        pending = [t for t in list(getattr(self, "_background", ()) or ()) if not t.done()]
        if not pending:
            return
        done, still = await asyncio.wait(pending, timeout=DRAIN_SECS)
        for t in still:
            t.cancel()
        if still:
            await asyncio.gather(*still, return_exceptions=True)
            log.warning("shutdown: %d background task(s) cut off after %.0f s", len(still), DRAIN_SECS)

    async def _voice_memory(self, memory: Optional["memory_mod.Memory"]) -> tuple[str, str, str]:
        """(conv, today, backend_today) for a new voice conversation. The two today blocks are built on
        a worker thread within TODAY_BUILD_SECS: a slow disk opens the conversation without them rather
        than holding the owner's first word."""
        if memory is None:
            return "", "", ""
        conv = memory.transcripts.new_conv("voice")

        def build() -> tuple[str, str]:
            return (memory.today(transcripts_mod.VOICE_CHARS, style="voice"),
                    memory.today(transcripts_mod.BACKEND_CHARS))

        try:
            today, backend_today = await asyncio.wait_for(asyncio.to_thread(build), timeout=TODAY_BUILD_SECS)
        except asyncio.TimeoutError:
            log.info("memory: today's lines took over %.0f ms — this conversation opens without them",
                     TODAY_BUILD_SECS * 1000)
            today = backend_today = ""
        except Exception as e:  # noqa: BLE001
            log.warning("memory: today's lines unavailable (%s)", type(e).__name__)
            today = backend_today = ""
        return conv, today, backend_today

    # ---- the memory bus and its sinks ----

    async def _start_memory_sinks(self) -> None:
        """Attach the opt-in sinks (memory_bus.configured). A sink that cannot start is logged and
        skipped: the daemon boots the same with the flags on and the modules absent or the port busy."""
        cfg = self._bus_cfg
        self.bus.register_service("/eve/memory/recall", lambda args: Daemon._recall_service(self, args))
        self.bus.subscribe("/eve/memory/remember", lambda topic, msg: Daemon._on_remember(self, msg))
        rosbridge_url, claude_url = "off", "off"
        if cfg.rosbridge:
            try:
                from .rosbridge import RosbridgeServer
                server = RosbridgeServer(self.bus, host=cfg.rosbridge_host, port=cfg.rosbridge_port)
                await server.start()
                self._rosbridge = server
                rosbridge_url = server.url
            except (ImportError, OSError) as e:
                log.warning("memory bus: rosbridge did not start (%s: %s)", type(e).__name__, e)
        if cfg.claude_mem:
            try:
                from .claude_mem import ClaudeMemMirror, ClaudeMemSink
                sink = ClaudeMemSink(self.bus, project="eve", url=cfg.claude_mem_url)
                sink.start()
                self._claude_mem_sink = sink
                claude_url = cfg.claude_mem_url or getattr(sink, "url", "on")
                if cfg.mirror:
                    mirror = ClaudeMemMirror(self.bus, url=cfg.claude_mem_url)
                    mirror.start()
                    self._claude_mem_mirror = mirror
            except (ImportError, OSError) as e:
                log.warning("memory bus: claude-mem did not start (%s: %s)", type(e).__name__, e)
        log.info("memory bus: rosbridge %s, claude-mem %s, mirror %s", rosbridge_url, claude_url,
                 "on" if self._claude_mem_mirror is not None else "off")

    async def _stop_memory_sinks(self) -> None:
        mirror, self._claude_mem_mirror = self._claude_mem_mirror, None
        if mirror is not None:
            await asyncio.to_thread(mirror.stop)
        sink, self._claude_mem_sink = self._claude_mem_sink, None
        if sink is not None:
            await asyncio.to_thread(sink.stop)
        server, self._rosbridge = self._rosbridge, None
        if server is not None:
            await server.stop()

    def _publish_observation(self, rec: Any) -> None:
        """A diary thought the diary KEPT. Never an unwritten candidate, never a photo."""
        self.bus.publish("/eve/memory/observation", {
            "thought": str(getattr(rec, "thought", "")),
            "observations": list(getattr(rec, "observations", []) or []),
            "changed": list(getattr(rec, "changed", []) or []),
            "tags": list(getattr(rec, "tags", []) or []),
            "importance": int(getattr(rec, "importance", 0) or 0),
            "novelty": int(getattr(rec, "novelty", 0) or 0),
            "time": float(getattr(rec, "ts", 0.0) or time.time()),
        })

    def _publish_lesson(self, action: str, stage: str, feedback: str) -> None:
        """A lesson moved on. Only the action, the stage, the lesson's topic and level, and eve's OWN
        feedback: the learner's ideas, strokes, images and spoken words never leave the lesson store."""
        topic = level = mode = lesson_id = ""
        server = getattr(self, "_learning_server", None)
        app = getattr(server, "app", None)
        try:
            active = getattr(app, "active", None)
            store = getattr(app, "store", None)
            if active and store is not None:
                summary = store.summary(store.get(active))
                topic, level = str(summary.get("topic") or ""), str(summary.get("level") or "")
                mode, lesson_id = str(summary.get("mode") or ""), str(summary.get("id") or "")
                stage = stage or str(summary.get("stage") or "")
        except Exception:  # noqa: BLE001 — a missing lesson is not an error worth a caption
            pass
        self.bus.publish("/eve/memory/lesson", {
            "action": action or "notice", "stage": stage or "", "topic": topic, "level": level,
            "mode": mode, "lesson_id": lesson_id, "feedback": " ".join(str(feedback or "").split()),
            "time": time.time(),
        })

    def _on_remember(self, msg: dict[str, Any]) -> None:
        """Someone on the bus asked eve to keep a line. eve never stars for itself, and a bus line is not
        something the owner said: it becomes one transcript line (channel "bus", a tool line) for the nightly
        dream to judge, beside everything else that day. With memory off it is dropped."""
        text = " ".join(str(msg.get("text") or "").split())[:500]
        if not text:
            return
        memory = getattr(self, "_memory", None)
        if memory is None:
            log.info("memory bus: remember dropped — memory is off")
            return
        title = " ".join(str(msg.get("title") or "").split())[:80]
        line = f"{title}: {text}" if title else text
        store = memory.transcripts
        try:
            conv = store.new_conv(transcripts_mod.BUS)
            kept = store.append(transcripts_mod.BUS, conv, "system", "tool", line, tool="remember")
            if kept:
                store.close(transcripts_mod.BUS, conv)
        except Exception as e:  # noqa: BLE001 - a bus line never costs the daemon
            log.warning("memory bus: remember not kept (%s)", type(e).__name__)
            return
        log.info("memory bus: remember -> %s", "a transcript line" if kept else "not written")

    async def _recall_service(self, args: dict[str, Any]) -> dict[str, Any]:
        """/eve/memory/recall: search the owner's claude-mem for eve's memories."""
        if self._claude_mem_sink is None:
            return {"results": [], "error": "claude-mem is off (EVE_CLAUDE_MEM=1)"}
        from .claude_mem import recall
        query = " ".join(str(args.get("query") or "").split())[:500]
        try:
            limit = max(1, min(int(args.get("limit", 5)), 20))
        except (TypeError, ValueError):
            limit = 5
        results = await asyncio.to_thread(recall, query, limit, self._bus_cfg.claude_mem_url)
        return {"results": list(results or [])}

    # ---- heartbeat loop ----

    async def _heartbeat_loop(self) -> None:
        while not self._shutdown.is_set():
            await self._push_heartbeat()
            try:
                await asyncio.wait_for(self._shutdown.wait(), timeout=HEARTBEAT_KEEPALIVE)
            except asyncio.TimeoutError:
                continue

    async def _push_heartbeat(self, force: bool = False) -> None:
        import json

        snap = build_heartbeat(self.state, codec=self._cjk_codec)
        # Dedup key is the Python-side serialized snapshot, not the wire bytes
        # — same dict means same content regardless of wire codec.
        serialized = json.dumps(snap, sort_keys=True, ensure_ascii=False)
        now = time.monotonic()
        changed = serialized != self._last_hb_serialized
        stale = (now - self._last_hb_sent_at) >= HEARTBEAT_KEEPALIVE
        if not (force or changed or stale):
            return
        if self.ble.connected:
            # Monitor build: what actually went on the wire for the agent
            # table. Keep this — "the board shows 0 sessions" has to be
            # separable from "the daemon sent 0 sessions", and reading that
            # off a photo of an e-ink panel is not a diagnosis.
            log.debug(
                "hb->board: total=%d running=%d waiting=%d agents=%s",
                snap["total"], snap["running"], snap["waiting"],
                [f"{a['n']}:{a['s']}" for a in snap.get("agents", [])],
            )
            log.debug(
                "heartbeat: %d bytes, entries=%d (last=%r), force=%s, changed=%s",
                len(serialized), len(snap.get("entries", [])),
                snap["entries"][-1] if snap.get("entries") else None,
                force, changed,
            )
            ok = await self.ble.send(snap, codec=self._cjk_codec)
            if ok:
                self._last_hb_serialized = serialized
                self._last_hb_sent_at = now
            else:
                log.warning("heartbeat: ble.send returned failure")

    async def _resync_board(self) -> None:
        """Re-run the on-connect resync after a board reboot on a live link:
        time sync, forced heartbeat, status poll. The board buffers serial RX
        during its boot-time panel clear, so no settling delay is needed."""
        log.info("board rebooted under a live link — resyncing time + state")
        await Daemon._send_resync(self)

    async def _send_resync(self) -> None:
        """What a board needs after a reboot or a (re)connect, in this order (test_vision pins it): the time,
        a forced heartbeat, a status poll, then listening, the agent's face, the camera and the sound. One
        copy for both paths, so a step added for one is never missing from the other. Called through the
        class: tests run these methods on a stub."""
        await self.ble.send(build_time_sync())
        await self._push_heartbeat(force=True)
        await self.ble.send({"cmd": "status"})
        await self._reset_listen()
        await self._resync_agent()
        await self._send_cam(True)
        await self._send_sound()

    async def _on_ble_connected(self) -> None:
        """On every (re)connect, emit time sync + force a heartbeat + kick
        a status poll so we learn the link's encryption state right away."""
        while not self._shutdown.is_set():
            await self.ble.wait_connected()
            # Guard against a stale connected-event: if we woke but the link is
            # already gone (the event was set but cleared a beat later, or a
            # teardown race), don't fire sends into a dead link and spin. Sleep
            # briefly and re-wait instead of looping with no delay.
            if not self.ble.connected:
                await asyncio.sleep(0.5)
                continue
            await Daemon._send_resync(self)
            self._apply_mic("the robot connected")
            # Wait for the connection to drop before waiting again.
            while self.ble.connected and not self._shutdown.is_set():
                await asyncio.sleep(1.0)
            self._apply_mic("the robot is not connected")

    # ---- the Mac microphone ----

    def _mic_wanted(self) -> bool:
        """The mic policy: the owner's switch is on, and the robot is connected
        (or EVE_MIC_ALWAYS=1 says to listen from boot)."""
        return self._mic.on and (self._ears_cfg.always or self.ble.connected)

    def _apply_mic(self, why: str) -> None:
        """Open or close the Mac microphone to match _mic_wanted. Called at boot,
        on every robot connect and drop, and when the owner flips the switch."""
        if self._ears is None:
            return
        want = self._mic_wanted()
        if want and not self._ears.listening:
            if not self._ears.start():
                log.warning("ears: the microphone did not open (%s); the wake word is off until it does", why)
        elif not want and self._ears.listening:
            self._ears.stop()
            log.info("ears: microphone closed — %s", why)

    def _mic_status(self) -> dict[str, Any]:
        listening = self._ears is not None and self._ears.listening
        return {"ok": True, "mic": "on" if self._mic.on else "off", "listening": listening,
                "connected": self.ble.connected, "always": self._ears_cfg.always,
                "available": self._ears is not None}

    async def _status_poller(self) -> None:
        """Poll the stick for status, and use the replies as a TX health check.

        The status ack is the only thing that proves host→board writes are
        landing. A half-dead link — reads fine, writes silently go nowhere —
        keeps RX_SILENCE_SECS happy (the board is still talking) while the board
        shows "No Claude connected" because no heartbeat ever reaches it.
        Missing acks are the only signal, so treat them as one.
        """
        POLL_INTERVAL = 60.0
        MISSED_LIMIT = 2          # ~2min of unanswered polls before reconnecting
        while not self._shutdown.is_set():
            try:
                await asyncio.wait_for(self._shutdown.wait(), timeout=POLL_INTERVAL)
                return
            except asyncio.TimeoutError:
                pass
            if not self.ble.connected:
                continue
            if self._status_sent_at is not None:
                self._status_missed += 1
                # A missed poll ends the clean streak: de-escalation needs
                # CONSECUTIVE answered polls, or a link that answers every
                # other poll would keep resetting the escalation.
                self._clean_polls = 0
                if self._status_missed >= MISSED_LIMIT:
                    self._status_missed = 0
                    self._status_sent_at = None
                    # Escalate: first a plain reconnect (which now holds the
                    # port closed for a while — the only host-side action that
                    # ever revived this wedge); if acks are STILL missing a
                    # cycle later, the wedge is board-side (its RX died while
                    # TX kept going) and we pulse RTS to reboot it. The
                    # escalation counter survives a lone post-reopen ack —
                    # the field pattern is a flap, not a clean recovery — and
                    # only a streak of clean polls (see the ack handler)
                    # resets it.
                    self._ack_escalation += 1
                    self._clean_polls = 0
                    log.info("ack escalation → %d", self._ack_escalation)
                    why = (f"no status ack for {MISSED_LIMIT} polls "
                           f"(~{int(MISSED_LIMIT * POLL_INTERVAL)}s) — "
                           "writes are not reaching the board")
                    if self._ack_escalation >= 2:
                        pulse = getattr(self.ble, "pulse_reset", None)
                        if pulse is not None:
                            self._ack_escalation = 0
                            self._clean_polls = 0
                            pulse("reconnect did not restore acks; " + why)
                            continue
                    force = getattr(self.ble, "force_reconnect", None)
                    if force is not None:
                        force(why)
                    continue
            self._status_sent_at = time.monotonic()
            # Re-sync time with every poll. The board's RTC-valid flag is
            # RAM-only, so any watchdog reset blanks the mini clock; a reboot
            # that slips past silence detection (fast reboot, half-dead link)
            # never refires the on-connect time sync, leaving the clock gone
            # until the next reconnect. Periodic sync makes it self-heal
            # within one poll and corrects RTC drift for free.
            await self.ble.send(build_time_sync())
            await self.ble.send({"cmd": "status"})

    # ---- host vision ----

    async def _send_cam(self, on: bool) -> None:
        """Ask the board to start/stop streaming frames. Only when a detector
        exists: a board streaming at a host that cannot look is wasted link
        time, and the board keeps its own tracking while no host asks."""
        if not self._vision.enabled or not self.ble.connected:
            return
        try:
            await asyncio.wait_for(self.ble.send(build_cam_cmd(on)), timeout=2.0)
        except asyncio.TimeoutError:
            log.warning("vision: cam %s command timed out", "on" if on else "off")

    # ---- sound (mute) ----

    async def _send_sound(self) -> None:
        """Tell the board the owner's choice. Its firmware gates every chirp on it."""
        if self.ble.connected:
            await self.ble.send(build_sound_cmd(self._sound.on))

    def _set_sound(self, on: bool) -> None:
        """The owner muted or unmuted eve (by voice or `eve-bridge sound`).
        Persisted, and the board is told at once. Motion and LEDs are untouched."""
        if self._sound.set(on):
            log.info("sound: %s (the head and lights keep moving)", "on" if on else "muted")
        if self.ble.connected:
            _spawn(self, self._send_sound())

    async def _vision_stats_loop(self) -> None:
        """One summary line per window, and only when frames arrived. Never per-frame."""
        while not self._shutdown.is_set():
            await asyncio.sleep(VISION_STATS_SECS)
            line = self._vision.stats_line()
            if line is not None:
                log.info(line)

    # ---- owner identity ----

    def _make_identity(self) -> Optional[FaceIdentity]:
        """Load the owner prints and wire the Vision describer. None when
        this host cannot produce feature prints — faces stay 'unknown'."""
        if not self._vision.enabled:
            return None
        describe = make_describer()
        if describe is None:
            return None
        core = OwnerIdentity(path=identity_default_path(), threshold=configured_threshold())
        n = core.load()
        log.info("identity: %d owner print(s) loaded from %s (threshold %.2f)", n, core.path, core.threshold)
        return FaceIdentity(core, describe=describe, listen_down=lambda: self._listen_down)

    def _handle_identity(self, action: str) -> dict[str, Any]:
        """Loop thread. The core is lock-guarded, so this is safe against a
        classify/enrol in flight on the vision executor."""
        if action == "reset":
            if self._identity is None:
                core = OwnerIdentity(path=identity_default_path())
                core.load()
                removed = core.reset()
            else:
                removed = self._identity.core.reset()
                self._identity.last_who = None
            log.info("identity: reset — %d owner print(s) removed", removed)
            return {"ok": True, "removed": removed}
        if self._identity is None:
            core = OwnerIdentity(path=identity_default_path(), threshold=configured_threshold())
            core.load()
            st = FaceIdentity(core, describe=lambda f, r: None).status()
        else:
            st = self._identity.status()
        return {"ok": True, "enabled": self._identity is not None, "identity": st}

    # ---- "hey eve" ----

    def _start_ears(self, loop: asyncio.AbstractEventLoop) -> None:
        if not self._ears_cfg.enabled:
            log.info("ears: disabled (EVE_VOICE=0)")
            return
        self._ears = Ears(self._ears_cfg, self._on_wake, loop, suppressed=self._wake_suppressed)
        # Load the model now so a missing one shows at boot; the mic itself opens
        # and closes in _apply_mic, to the policy in _mic_wanted.
        if not self._ears.prepare():
            self._ears = None
            return
        if not self._mic.on:
            log.info("ears: microphone off (owner's choice, %s) — `eve-bridge mic on` turns it back on",
                     self._mic.path)
        elif not self._ears_cfg.always:
            log.info("ears: the microphone opens when the robot connects and closes when it leaves "
                     "(EVE_MIC_ALWAYS=1 keeps it open)")
        self._apply_mic("boot")
        if not voice_agent.live_available():
            log.warning("voice: OPENAI_API_KEY not set — eve will hear its name but the desk voice (OpenAI's Live "
                        "API, not yet on OpenRouter) cannot talk back; everything else runs on OPENROUTER_API_KEY")
        log.info("agent: Codex Computer Use %s; app permissions are requested by Codex per task",
                 "ready" if self._agent_cfg.enabled else "disabled (EVE_COMPUTER_CONTROL=0)")

    def _wake_suppressed(self) -> bool:
        """Reasons not to wake: the human is dictating, a conversation is
        already open, or the board's button is held to talk. The button is
        the board's last ptt edge, not the desk call's ``held``: a call that
        ends mid-hold drops ``held``, and a press during a conversation never
        reaches the desk (verification/Eve/DeskCall.lean)."""
        return (self._listen_down
                or (self._conversation is not None and not self._conversation.done())
                or getattr(self, "_ptt_held", False))

    def _desk_calls_get(self):
        """The board's hold-to-talk calls (desk_call.py), made on the first press: they borrow the Telegram
        chat as the brain and the phone call's OpenAI voice, and wait while the phone is on a call. Each call
        speaks on the Voice PE when the controller is connected as it starts, else on the Mac."""
        if getattr(self, "_desk_calls", None) is None:
            from .desk_call import BoardSpeaker, DeskCalls, Speaker
            from .phone_call import make_voice

            def phone_busy() -> bool:
                # Only a phone call that has the chat and is using it (phone_call.IDLE_YIELD_SECS): an open, unused
                # call must not hold the Voice PE's button.
                server = getattr(getattr(self, "_miniapp", None), "server", None)
                calls = getattr(server, "calls", None)
                if calls is None or getattr(calls, "active", None) is None:
                    return False
                holding = getattr(calls, "holding_chat", None)
                return holding() if callable(holding) else True

            def speaker():
                # eve's reply on the Voice PE when it is the connected controller, else on the Mac.
                ctl = getattr(self, "_controller", None)
                if ctl is not None and ctl.connected:
                    return BoardSpeaker(ctl.send_audio)
                return Speaker()

            self._desk_calls = DeskCalls(getattr(self, "_telegram", None), make_voice(), self.ble.send,
                                         mic_device=self._ears_cfg.device, busy=phone_busy,
                                         speaker_factory=speaker,
                                         # a desk call is a conversation: eve follows whoever is talking
                                         on_phase=self._follower.on_phase)
        return self._desk_calls

    def _on_wake(self, keyword: str) -> None:
        log.info("ears: heard %r", keyword)
        self._note_activity()          # a conversation is activity: the explorer stops
        if self._conversation is not None and not self._conversation.done():
            return
        # A manual explore ignores activity, so end it here: the board
        # cannot pan the room and hold the conversation pose at once.
        _spawn(self, self._dismiss_explore("wake word"))
        # "lesson" on its own: the conversation opens with the lesson already requested.
        lesson_word = getattr(getattr(self, "_ears_cfg", None), "lesson_word", "")
        lesson = bool(lesson_word) and keyword == keyword_id(lesson_word)
        conversation = self._converse(lesson_wake=True) if lesson else self._converse()
        self._conversation = asyncio.create_task(conversation, name="voice-conversation")

    async def _voice_gate_for(self, think_aloud: Optional[dict[str, Any]]) -> Optional[Any]:
        """EVE_VOICE_GATE=shadow|on: this conversation's speaker gate, enrolled from the audio that woke
        eve (voice_gate.py). None — today's behaviour — when it is off, when the conversation was opened
        without a wake word, or for a think-aloud lesson, where eve takes notes on whoever speaks. The model
        loads once, off the loop; anything wrong with it is simply "no gate"."""
        from . import voice_gate

        ears = getattr(self, "_ears", None)
        wake = getattr(ears, "last_wake_audio", b"") or b""
        if ears is not None and wake:
            ears.last_wake_audio = b""                   # one conversation's enrolment, then gone
        cfg = voice_gate.configured()
        if cfg.mode == "off" or think_aloud is not None or not wake:
            return None

        def make() -> Optional[Any]:
            if getattr(self, "_voice_embedder", None) is None:
                self._voice_embedder = voice_gate.SherpaEmbedder(cfg.model)
            return voice_gate.build(cfg, wake, embedder=self._voice_embedder)

        try:
            return await asyncio.to_thread(make)
        except Exception as e:  # noqa: BLE001 — no model file, a bad one: eve hears as it always has
            log.warning("voice gate: off for this conversation (%s: %s)", type(e).__name__, e)
            return None

    def _head_pose_asker(self) -> Optional[Any]:
        """EVE_HEAD_MODEL=jev: "look left" is decided by Jev, asked in its own idiom (typed_ask.py), in a
        quarter of a second instead of ~3.2 s through the backend. Off by default: the words of a turn that
        mentions a direction leave the Mac. Built once; anything wrong with it is simply "off"."""
        if getattr(self, "_head_asker", None) is None:
            self._head_asker = False
            if (os.environ.get("EVE_HEAD_MODEL") or "").strip().lower() == "jev":
                try:
                    from . import jev, typed_ask

                    url, key, model = jev.route_config(os.environ)
                    sync = typed_ask.make_jev_head_asker(jev.make_predict(url, key, model, timeout_s=1.5),
                                                         time.perf_counter)

                    async def ask(text: str) -> str:          # a network call: never on the loop
                        return await asyncio.to_thread(sync, text)

                    self._head_asker = ask
                    log.info("head: poses are chosen by Jev (EVE_HEAD_MODEL); the backend remains the fallback")
                except Exception as e:  # noqa: BLE001
                    log.warning("head: the pose model is off (%s: %s)", type(e).__name__, e)
        return self._head_asker or None

    async def _resync_agent(self) -> None:
        """Tell a (re)connected or rebooted board which conversation phase is live —
        idle when none. The board drops a phase that goes 30 s without a word from
        us, so a lost 'idle' can no longer pin the listening pose (bench 2026-09-06)."""
        if self.ble.connected:
            await self.ble.send({"cmd": "agent", "state": self._agent_state})

    async def _agent_keepalive(self) -> None:
        """While a conversation is open, repeat the current phase every 10 s."""
        while True:
            await asyncio.sleep(10.0)
            await self._resync_agent()

    async def _converse(self, think_aloud: Optional[dict[str, Any]] = None, lesson_wake: bool = False) -> None:
        # getattr: the daemon tests stand in a bare object for the ears.
        if self._ears is None or not getattr(self._ears, "listening", True):
            return
        server = getattr(self, "_learning_server", None)
        if lesson_wake and server is not None:
            # The word itself is the request: open the lesson window now, with no model in the loop.
            # The conversation then asks learn-a-topic or bring-a-problem and starts the lesson.
            try:
                await asyncio.to_thread(server.app.voice, action="open")
            except Exception as e:  # noqa: BLE001
                log.warning("lesson word: could not open the lesson window: %s: %s", type(e).__name__, e)
        # What eve remembers of talking with the owner (memory.py, recall.py), when memory is on: the
        # opening brief (one clause), this conversation's transcript id, and today's lines for the voice and
        # its backend. Read here because the prompt is built once, at session.start, before the owner has
        # finished their first sentence, and before the microphone is taken, so a hush during the read
        # leaves nothing to undo. The session reads the profile itself. Memory off: all empty.
        memory = getattr(self, "_memory", None)
        brief = recall_mod.opening_brief(self._recall_cfg, memory.transcripts if memory is not None else None)
        if brief:
            log.info("recall: an opening brief of %d chars", len(brief))
        conv, today, backend_today = await Daemon._voice_memory(self, memory)
        self._voice_conv = conv
        mic = self._ears.subscribe()
        keepalive = asyncio.create_task(self._agent_keepalive(), name="agent-keepalive")
        gate = await Daemon._voice_gate_for(self, think_aloud)
        try:
            await voice_agent.open_session(mic, self._on_agent_state, self._make_agent,
                                           config=self._voice_cfg, agent_enabled=self._agent_cfg.enabled,
                                           on_caption=self._on_caption, on_explore=self._on_voice_explore,
                                           on_expression=lambda who, text: Daemon._on_expression_text(self, who, text),
                                           scene=self._scene, head=self._head, intent=self._intent,
                                           on_sound=self._set_sound, muted=lambda: self._sound.muted,
                                           thinker=self._thinker, on_photo=self._photo_for_owner,
                                           brief=brief, memory=memory, conv=conv, today=today,
                                           backend_today=backend_today,
                                           learning=server.app.voice if server is not None else None,
                                           think_aloud=think_aloud, lesson_wake=lesson_wake,
                                           head_pose=Daemon._head_pose_asker(self), gate=gate,
                                           on_spoken_idea=server.app.append_spoken if server is not None else None,
                                           on_think_aloud=lambda on, lesson_id: Daemon._on_think_aloud(
                                               self, on, lesson_id),
                                           on_open=lambda session: Daemon._on_voice_session(self, session),
                                           mac_busy=lambda: Daemon._texted_task_running(self),
                                           meeter=getattr(self, "_meeter", None),
                                           **Daemon._app_hubs(self, tools_only=True))
        except asyncio.CancelledError:
            self._explore_after_conversation = None      # hushed: stay put
            self._think_aloud_wish = None                # a hush or a stop cancels a pending listen too
            raise
        except Exception as e:  # noqa: BLE001
            log.warning("voice: conversation failed: %s: %s", type(e).__name__, e)
            self._on_agent_state("error")
        finally:
            keepalive.cancel()
            await asyncio.gather(keepalive, return_exceptions=True)
            self._ears.unsubscribe(mic)                  # the microphone stops reaching the session here
            if gate is not None:
                log.info("voice gate: %s", gate.summary())   # counts and seconds only: never words, never audio
                gate.reset()                             # the enrolled voice does not outlive the conversation
            self._voice_session = None
            if getattr(self, "_think_aloud_state", "off") != "off":
                Daemon._on_think_aloud(self, False, None)
            chief = getattr(getattr(self, "_telegram", None), "chief", None)
            if chief is not None:
                try:
                    chief.breakpoint("wake_end")         # a held reminder or a queued Mac step may go now
                except Exception as e:  # noqa: BLE001 — the chief never costs the conversation its close
                    log.warning("chief: the wake breakpoint failed (%s)", type(e).__name__)
            self._on_agent_state("idle")
            wish = getattr(self, "_think_aloud_wish", None)
            if wish is not None and not self._shutdown.is_set():
                # Listening was asked for while this conversation was closing: open it now.
                self._think_aloud_wish = None
                self._start_think_aloud_conversation(wish)
            reason, self._explore_after_conversation = self._explore_after_conversation, None
            if reason is not None:
                await self._request_explore(reason)

    def _room_notes_taker(self) -> RoomNotes:
        if self._room_notes is None:
            self._room_notes = RoomNotes(self._recall_cfg, notes_configured(), make_notes_client(),
                                         self._ears, on_state=self._on_agent_state)
        return self._room_notes

    def _phone_notes(self, inlet: Any) -> Any:
        """Notes on the phone (phone_call.PhoneNotes): each session is a RoomNotes of its own, never the desk's, so
        the two never share a recording, fed from the call and written beside the desk's as ``source:
        eve-link``. The notes are texted to the owner the way the Meet notetaker's are: the history gets
        ``about``, never the words heard (the transcript is anyone's, and the chat reading it as its own would
        be a stored prompt injection)."""
        from .phone_call import PhoneNotes

        async def notify(text: str, about: str = "") -> None:
            await inlet.tell_owner(text, about=about, title=telegram_mod.NOTES_TITLE)

        return PhoneNotes(lambda mic: RoomNotes(self._recall_cfg, notes_configured(), make_notes_client(), mic,
                                                source="eve-link"),
                          notify)

    def _on_voice_explore(self) -> None:
        """The voice tool go_explore: remember the wish; it is granted in
        _converse once the conversation has closed."""
        self._explore_after_conversation = "requested by voice"

    @staticmethod
    async def _type_into_terminal(cwd: str, text: str) -> str:
        """"claude: <text>" from the phone: raise the session's terminal (focus_terminal.py) and type the
        line with Return, through System Events. Real keystrokes into whatever is then frontmost — which is
        why it only runs while the owner has said "claude on"."""
        from .focus_terminal import _osascript, focus_session_terminal

        await focus_session_terminal(cwd or "")
        await asyncio.sleep(0.3)
        escaped = text.replace("\\", "\\\\").replace('"', '\\"')
        script = f'tell application "System Events" to keystroke "{escaped}"\ntell application "System Events" to keystroke return'
        if await _osascript(script) is None:
            return "I couldn't type into the terminal (System Events refused — check Automation permissions)."
        return ""                                         # typed: the phone reacts to the owner's message

    def _texted_task_running(self) -> bool:
        inlet = getattr(self, "_telegram", None)
        return inlet is not None and inlet.task_running

    def _desk_has_the_mac(self) -> bool:
        """For the text door: a spoken conversation is open, or a task it started is still running. One
        agent on the mouse at a time, and the person at the desk wins."""
        conversation = getattr(self, "_conversation", None)
        agent = getattr(self, "_active_agent", None)
        inlet = getattr(self, "_telegram", None)
        spoken_task = agent is not None and agent.running and not (inlet is not None and inlet.task_running)
        return (conversation is not None and not conversation.done()) or spoken_task

    def _make_apps(self, owner_ids: frozenset[int]) -> Optional[Any]:
        """EVE_COMPOSIO=1 with a key: the owner's Composio session (composio_tools.py), started on a
        thread so a slow or down Composio never holds the daemon; the text brain offers its tools once it is
        up. None otherwise. Gmail read only, the calendar writable, the rest asked first (the module)."""
        from . import composio_tools

        cfg = composio_tools.configured(os.environ, owner_ids)
        if not cfg.enabled:
            log.info("apps: off (EVE_COMPOSIO)")
            return None
        bridge = composio_tools.ComposioBridge(cfg)

        def start() -> None:
            try:
                bridge.start()
                log.info("apps: Composio session up for %s; policy %s", cfg.user_id, composio_tools.toolkit_policy())
            except Exception as e:  # noqa: BLE001 — the text brain simply has no app tools this run
                log.warning("apps: Composio did not start (%s: %s)", type(e).__name__, e)

        threading.Thread(target=start, name="composio-start", daemon=True).start()
        return bridge

    def _make_direct(self) -> Optional[Any]:
        """EVE_APPS=1: the owner's direct apps (apps_mcp.py): Gmail, Calendar and Drive through Google's
        APIs, GitHub and Maps through their MCP servers, each up only when signed in (``apps login google``, the
        env file's tokens). Started on a thread so a slow or down server never holds the daemon; the text brain
        offers them once up. None otherwise. Composio (``_make_apps``) keeps Slack, Notion and Linear."""
        from . import apps_mcp

        cfg = apps_mcp.configured(os.environ)
        if not cfg.enabled:
            log.info("apps: direct off (EVE_APPS)")
            return None
        bridge = apps_mcp.AppsBridge(cfg)
        if not bridge.available():
            log.info("apps: direct on, but nothing is signed in (eve-bridge apps login google)")

        def start() -> None:
            try:
                bridge.start()
                log.info("apps: direct up: %s; policy %s", ", ".join(bridge.apps) or "none", bridge.policy)
            except Exception as e:  # noqa: BLE001 — the text brain simply has no direct app tools this run
                log.warning("apps: direct did not start (%s)", type(e).__name__)

        threading.Thread(target=start, name="apps-direct-start", daemon=True).start()
        return bridge

    def _make_telegram(self) -> Optional["telegram_mod.TelegramInlet"]:
        """EVE_TELEGRAM=1 with a token and an owner id: the inlet, lent the same agent factory, camera,
        slow brain and memory (the same Memory, None while memory is off) the voice session gets, the owner's
        apps (direct: Google, GitHub, Maps; Composio: Slack, Notion, Linear) and their second brain (the vault).
        None — today's behaviour — otherwise."""
        from . import opentable, second_brain

        memory = getattr(self, "_memory", None)

        tg = telegram_mod.configured()
        if tg.enabled:
            log.info("telegram: web search %s", tg.search.engine)
        vault = second_brain.configured()
        if tg.enabled:
            log.info("second brain: %s", f"on at {vault.root}" if vault.enabled else "off (EVE_SECOND_BRAIN)")
        # The watcher texts through this door, so it exists only with it. Made here, started beside the door.
        from . import watch as watch_mod

        try:
            watcher = watch_mod.make_watcher() if tg.enabled else None
        except Exception:  # noqa: BLE001 — a watcher that cannot start costs the watching, never the daemon
            log.exception("watch: could not start; watching is off this run")
            watcher = None
        self._watcher = watcher
        apps = Daemon._make_apps(self, tg.owner_ids) if tg.enabled else None
        direct = Daemon._make_direct(self) if tg.enabled else None
        self._meeter = Daemon._make_meeter(self, direct) if tg.enabled else None
        self._agentcard = (Daemon._make_agentcard(self)
                           if tg.enabled and openrouter.available() else None)
        # The chat log (chat_log.py): on for every install unless EVE_CHAT_LOG=0, memory on or off, so a quiet
        # spell or a restart never empties the chat's history. Forget reaches it through the Memory, when one is lent.
        from . import chat_log as chat_log_mod

        chat_log = chat_log_mod.open_configured() if tg.enabled else None
        if chat_log is not None and memory is not None:
            memory.chat_log = chat_log
        inlet = telegram_mod.make_inlet(
            tg, watcher=watcher, meeter=self._meeter, **Daemon._app_hubs(self, tools_only=True),
            canvas=Daemon._make_canvas(self),
            share=Daemon._make_share(self) if tg.enabled else None,
            agentcard=self._agentcard,
            america=(Daemon._make_america(self)
                     if tg.enabled and openrouter.available() else None),
            opentable_enabled=opentable.configured(),
            apps=apps, direct=direct,
            vault=vault if tg.enabled and vault.enabled else None,
            agent_factory=self._make_agent, agent_enabled=self._agent_cfg.enabled,
            busy=lambda: Daemon._desk_has_the_mac(self),
            brief=lambda: recall_mod.opening_brief(self._recall_cfg, memory.transcripts if memory is not None else None),
            memory=memory, chat_log=chat_log,
            on_photo=self._photo_for_owner, thinker=self._thinker,
            on_state=self._on_agent_state,
            scene=self._scene, head=self._head, on_explore=lambda: self._request_explore("requested from Telegram"),
            on_sound=self._set_sound, on_caption=self._on_caption,
            notes=lambda: self._room_notes_taker(), terminal=Daemon._type_into_terminal,
            # Live sessions only: one whose terminal died without a SessionEnd is dropped (claude_live).
            # Awaited by the inlet: the ps/lsof probe runs on a worker thread, never on this loop.
            claude_sessions=lambda: claude_live.picker_sessions_off_loop(self.state))
        if inlet is not None:
            inlet.chief = Daemon._make_chief(self, inlet, watcher)
        elif self._agentcard is not None:
            self._agentcard.close()
            self._agentcard = None
        return inlet

    def _make_chief(self, inlet: Any, watcher: Any) -> Any:
        """The chief of staff (chief.py) when EVE_CHIEF has it on, else None (today's behaviour: the door is
        byte for byte what it was). It is built from the door itself: a Mac step runs through the door's task slot
        (chief_start, chief_stop, chief_mac_free), a Go is the door's question (chief_ask), a message goes through
        tell_owner (chief_notify), and the options call through the door's own Responses creator. A chief that
        cannot be built costs the chief, never the door."""
        from . import chief as chief_mod

        if not chief_mod.enabled():
            log.info("chief: off (EVE_CHIEF=%s)", chief_mod.mode())
            return None
        try:
            chief = chief_mod.make_chief(start_task=inlet.chief_start, stop_task=inlet.chief_stop,
                                         mac_free=inlet.chief_mac_free, watcher=watcher, create=inlet._create,
                                         ask=inlet.chief_ask, notify=inlet.chief_notify)
        except Exception:  # noqa: BLE001
            log.exception("chief: could not start; it is off this run")
            return None
        if chief is not None:
            log.info("chief: on (EVE_CHIEF=%s); %d open card(s)", chief_mod.mode(), len(chief.ledger.open()))
        return chief

    @staticmethod
    def _warn_removed_computer(environ: Optional[Any] = None) -> bool:
        """EVE_COMPUTER once chose the desktop executor (Codex or Holo). Holo was removed on 2026-09-30, so
        Codex is the only one: a leftover EVE_COMPUTER=holo logs one warning and tasks run on Codex."""
        env = os.environ if environ is None else environ
        if (env.get("EVE_COMPUTER") or "").strip().lower() != "holo":
            return False
        log.warning("agent: EVE_COMPUTER=holo is set, but Holo was removed (2026-09-30); tasks run on Codex")
        return True

    def _make_agent(self, on_event: Any, ask_user: Any, *, floor: Optional[str] = None) -> Any:
        """Codex computer use, behind the launch reflex (app_reflex.py): "open Spotify" is `open -a`,
        with Jev for wording the rules do not know; everything else is Codex's.

        ``floor="codex"`` (the chief's one-way act, 2026-09-29): Codex and nothing else: no reflex and no other
        body (the web reader, the Chrome lane), so the act the owner approved goes whole to the agent that asks
        before a consequential step."""
        from . import app_reflex

        warm = getattr(self, "_codex_warm", None)       # codex_warm.py: a Codex agent already started
        make_inner = ((lambda: warm.take(on_event, ask_user)) if warm is not None
                      else (lambda: CodexComputerAgent(on_event=on_event, ask_user=ask_user)))
        if floor == "codex":
            agent = app_reflex.ReflexFirstAgent(make_inner, on_event, enabled=False,
                                                on_done=warm.kick if warm is not None else (lambda: None))
            self._active_agent = agent
            return agent
        agent = app_reflex.ReflexFirstAgent(make_inner, on_event, asker=app_reflex.jev_asker(),
                                            quit_asker=app_reflex.jev_quit_asker(),
                                            enabled=app_reflex.reflexes_on(),
                                            on_done=warm.kick if warm is not None else (lambda: None),
                                            desk=Daemon._desk(self),
                                            **Daemon._bodies(self, make_inner, on_event, ask_user))
        self._active_agent = agent
        return agent

    def _start_miniapp(self, tasks: list) -> None:
        from . import miniapp

        cfg = miniapp.configured()
        if not cfg.enabled:
            return
        try:
            import anthropic  # noqa: F401 — the import is the check
        except ImportError:
            log.warning("miniapp: EVE_MINIAPP is on but the anthropic SDK is not installed "
                        "(pip install -e \".[miniapp]\"); off")
            return
        self._miniapp = miniapp.MiniApp(cfg)
        # "Call eve" in the Mini App (phone_call.py): push to talk into the Telegram chat's own brain, every
        # reply read back. It needs the chat (the brain) and the OpenRouter key (the ears and the voice).
        from .phone_call import PhoneCalls, make_voice

        server = getattr(self._miniapp, "server", None)
        if server is not None:
            # eve's apps (app_registry.APPS): the Mini App's Lights, Music and TV cards and Eve Link's Apps, each
            # the hub the chat and the voice share, or None when it is off or not set up on this Mac
            for slot, hub in Daemon._app_hubs(self).items():
                setattr(server, slot, hub)
            from .link_memory import MemoryPanel

            # the Memory card (link_memory.py): the memory is built after this, on a thread, so it is read per call
            server.memory = MemoryPanel(lambda: getattr(self, "_memory", None))
            from .link_notes import NotesPanel

            server.notes = NotesPanel()                      # Notes, the owner's vault (link_notes.py, second_brain.py)
            server.calls = PhoneCalls(
                lambda init_data: miniapp.check_init_data(init_data, cfg.token, cfg.owner_ids),
                getattr(self, "_telegram", None), make_voice())
            # Eve Link's chat (link_chat.py): the owner texts the chat's brain from the app, with the link token.
            # The Work tab's Claude Code sessions in ~/Documents/personal/work (work_sessions.py,
            # docs/work-sessions.md); EVE_WORK_SESSIONS=0 leaves them off and the routes answer 503.
            from . import work_sessions
            from .link_chat import LinkChat

            if work_sessions.enabled():
                self._work = work_sessions.WorkSessions(state=lambda: self.state)
            server.link = LinkChat(lambda: getattr(self, "_telegram", None),
                                   work=lambda: getattr(self, "_work", None))
            # Notes on the phone (owner, 2026-10-04: "the phone should hold the port until i click action button
            # to turn it off, this allows me to take notes on the go"): take_notes said on an Eve Link call records
            # the phone through the room's own note-taker, and the notes are texted through the chat's door.
            inlet = getattr(self, "_telegram", None)
            if inlet is not None:
                # eve's own pictures and files are kept in the app door's store and shown in its chat, from the
                # start, not only once the app has called (TelegramInlet._send_media)
                inlet.link_uploads = server.link.uploads
                server.calls.notes = Daemon._phone_notes(self, inlet)
                inlet._phone_notes = server.calls.notes_action
            Daemon._start_phone_expressions(self, server.calls, tasks)
        tasks.append(asyncio.create_task(self._miniapp.run(), name="miniapp"))
        if getattr(self, "_telegram", None) is not None:
            # "make me a habit tracker" texted to eve builds an app (apps_maker.ChatMaker); its Open button
            # is sent to the chat when the build is done.
            from . import apps_maker

            inlet = self._telegram
            door = apps_maker.ChatMaker(self._miniapp, lambda coro, name: inlet._spawn(coro, name))
            # One set of apps-being-changed for both doors, so a delete, rename or undo from either one waits
            # for a build started from the other.
            door.building = self._miniapp.changing
            self._telegram._maker = door

    def _make_chrome_lane(self) -> Any:
        """EVE_BROWSER_ATTACH=1: the browser lane attached to the owner's own Chrome, kept for the daemon's
        life (connected on the first web task, not now). EVE_BROWSER_OWN=1: the same lane on eve's own
        Chrome instead (browser_lane.py, EVE'S OWN CHROME). None otherwise, or without Playwright."""
        from . import browser_lane

        cfg = browser_lane.configured()
        if not cfg.attach:
            return None
        try:
            import playwright  # noqa: F401 — the import is the check
        except ImportError:
            log.warning("chrome lane: EVE_BROWSER_ATTACH is on but Playwright is not installed; off")
            return None
        # Jev grounds each click unless the owner turned it off (EVE_JEV_STEP=0).
        env = {**os.environ, "EVE_JEV_STEP": os.environ.get("EVE_JEV_STEP", "1")}
        lane = browser_lane.BrowserLane(cfg, step_asker=browser_lane.make_step_asker(env))
        from . import chrome_consent

        if cfg.steel:
            log.info("chrome lane: on — web tasks try eve's Steel browser first (local Steel, no key, "
                     "signed in with `eve-bridge steel-profile`); Codex is the floor; no Allow to press")
            return lane
        if cfg.own:
            log.info("chrome lane: on — web tasks try eve's own Chrome first (%s, signed in with "
                     "`eve-bridge chrome-profile`); Codex is the floor; no Allow to press", cfg.own_dir)
            return lane
        log.info("chrome lane: on — web tasks try the owner's logged-in Chrome first (profile: %s); Codex is the "
                 "floor; Chrome's Allow: %s", cfg.chrome_profile or "the first open",
                 "pressed by eve (EVE_CHROME_ACCESS=allow)" if chrome_consent.access_preference() == "allow"
                 else "asked on the phone")
        return lane

    def _app_hub(self, app: app_registry.App) -> Any:
        """One of eve's apps (app_registry.APPS), made once and shared by the chat, the voice, the Mini App and Eve
        Link; None when it is switched off or not set up (no lights file, no login, never paired, no camera). The
        module's factory reads its own switch and files at the first ask, so a restart picks up a new scan. Making
        one sends nothing to its device: the first command connects."""
        attr = "_" + app.slot
        if not hasattr(self, attr):
            try:
                hub = getattr(app.mod(), app.make)()
            except Exception:  # noqa: BLE001 — an app that cannot load costs that app, never the daemon
                log.exception("%s: could not load; it is off this run", app.slot)
                hub = None
            setattr(self, attr, hub)
            ctl = getattr(self, "_controller", None)
            if app.slot == "spotify" and hub is not None and ctl is not None:
                # Spotify mode on the Voice PE: its ring green, its dial the volume, its clicks the transport
                from .music_mode import MusicMode

                self._music_mode = MusicMode(hub, ctl.show, say=Daemon._board_say(self))
                hub.mode = self._music_mode
        return getattr(self, attr)

    def _app_hubs(self, tools_only: bool = False) -> dict[str, Any]:
        """Every app's hub by slot, in tool order (app_registry.APPS); ``tools_only``: the apps that give the models
        tools (the chat's and the voice's kwargs). A ``_<slot>_hub`` method, where one is kept, is called instead,
        so a test that replaces it still decides that app."""
        apps = app_registry.with_tools() if tools_only else app_registry.APPS
        out: dict[str, Any] = {}
        for app in apps:
            own = getattr(Daemon, f"_{app.slot}_hub", None)
            out[app.slot] = own(self) if own is not None else Daemon._app_hub(self, app)
        return out

    # The lights', Spotify's and the TV's own names, kept for the tests that replace them (one line each).
    def _lights_hub(self) -> Any:
        return Daemon._app_hub(self, app_registry.get("lights"))

    def _spotify_hub(self) -> Any:
        return Daemon._app_hub(self, app_registry.get("spotify"))

    def _firetv_hub(self) -> Any:
        return Daemon._app_hub(self, app_registry.get("firetv"))

    def _make_canvas(self) -> Any:
        """The owner's Canvas (canvas.py), read only, for the Telegram brain; None when CANVAS_BASE_URL or
        CANVAS_API_TOKEN is unset. Made once per daemon."""
        if not hasattr(self, "_canvas"):
            from . import canvas as canvas_mod

            try:
                self._canvas = canvas_mod.make_canvas()
            except Exception:  # noqa: BLE001 — a Canvas that cannot load costs Canvas, never the daemon
                log.exception("canvas: could not load; Canvas is off this run")
                self._canvas = None
        return self._canvas

    def _make_share(self) -> Any:
        """Sharing the owner's files with someone (share.py): iMessage on this Mac, and email as a Drive link when
        the direct apps' Google sign-in can write to Drive (share.google_token, a file check, no network). None
        unless EVE_SHARE=1. Made once per daemon (2026-10-04)."""
        if not hasattr(self, "_share"):
            from . import share as share_mod

            try:
                self._share = share_mod.make_share(token=share_mod.google_token())
            except Exception:  # noqa: BLE001 — sharing that cannot load costs sharing, never the daemon
                log.exception("share: could not load; sharing is off this run")
                self._share = None
        return self._share

    def _make_agentcard(self) -> Any:
        """Optional user-scoped purchasing for the shared chat/call brain. No live request at startup."""
        from . import agentcard

        try:
            return agentcard.make_agentcard()
        except Exception as exc:  # noqa: BLE001 — credentials and provider bodies must never enter logs
            log.warning("agentcard: could not load (%s); purchasing is off this run", type(exc).__name__)
            return None

    def _make_america(self) -> Any:
        """Optional America.gov legal information for the shared chat/call brain; no startup request."""
        if not hasattr(self, "_america"):
            from . import america

            try:
                self._america = america.make_america()
            except Exception as exc:  # noqa: BLE001 — keep provider data and credentials out of logs
                log.warning("america: could not load (%s); legal information is off this run", type(exc).__name__)
                self._america = None
        return self._america

    def _board_say(self) -> Any:
        """A few words on the Voice PE's speaker (the device picker's names), with the call voice; None without
        one (no OPENROUTER_API_KEY): the picker then shows the ring alone."""
        from .phone_call import make_voice

        voice = make_voice()
        if voice is None:
            return None

        async def say(text: str) -> None:
            ctl = getattr(self, "_controller", None)
            if not text or ctl is None or not ctl.connected:
                return
            from .desk_call import BoardSpeaker

            pcm = await asyncio.to_thread(lambda: b"".join(voice.speak_stream(text)))
            speaker = BoardSpeaker(ctl.send_audio)
            await speaker.flush()                         # a name not yet finished gives way to the next
            await speaker.play(pcm)
        return say

    def _make_meeter(self, apps: Any) -> Any:
        """The Meet notetaker (meet.py): joins from the owner's Chrome (attach mode) on a browser lane of its own,
        reads the owner's calendar through the direct Calendar (apps_mcp, Google's API) for "join my 3pm", and writes the notes beside
        the room notes. Chrome's "Allow remote debugging?" for its connection goes through the same broker as the
        Chrome lane's. None when it is off or cannot run."""
        from . import chrome_consent
        from . import meet as meet_mod
        from .notes import notes_dir

        broker = chrome_consent.ConsentBroker(
            lambda q: Daemon._ask_owner_on_phone(self, q),
            tell_owner=lambda text: Daemon._tell_owner_on_phone(self, text),
            auto_allow=chrome_consent.access_preference() == "allow")
        try:
            return meet_mod.make_meeter(None, notes_dir(self._recall_cfg), answer=broker.answer_own_connection,
                                        apps=apps)
        except Exception:  # noqa: BLE001 — a notetaker that cannot start costs the meetings, never the daemon
            log.exception("meet: could not start; joining meetings is off this run")
            return None

    async def _ask_owner_on_phone(self, question: str) -> str:
        """A yes/no for the owner over Telegram (chrome_consent.py). Raises when there is no way to ask."""
        inlet = getattr(self, "_telegram", None)
        chat = getattr(inlet, "_chat_id", None) if inlet is not None else None
        if inlet is None or chat is None:
            raise RuntimeError("no Telegram chat to ask the owner in")
        if getattr(inlet, "_awaiting_answer", False):
            # Another question is waiting (a task's, a Claude permission): asking now would take its answer.
            raise RuntimeError("another question is already waiting for the owner")
        return await inlet._ask_user(question, chat, title="Chrome access")

    async def _tell_owner_on_phone(self, text: str) -> None:
        inlet = getattr(self, "_telegram", None)
        chat = getattr(inlet, "_chat_id", None) if inlet is not None else None
        if inlet is not None and chat is not None:
            await inlet._say(chat, text)

    def _desk(self) -> Any:
        """desk_lane.DeskLane.attempt, made once for the daemon's life, or None with EVE_DESK_LANE=0."""
        from . import desk_lane

        if not desk_lane.enabled():
            return None
        if getattr(self, "_desk_lane", None) is None:
            self._desk_lane = desk_lane.DeskLane()
            log.info("desk lane: on — keyword-decided clicks on the frontmost window before Codex")
        return self._desk_lane.attempt

    def _bodies(self, make_inner: Any, on_event: Any, ask_user: Any) -> dict[str, Any]:
        """ReflexFirstAgent's bodies beside Codex, and the one router that picks between them: Jev first
        (browser_router.py: the web reader for a public-web reading task, web_reader.py), then the Chrome lane's rule
        for a web goal, else Codex. {} when neither extra body is on: every task stays Codex's."""
        from . import web_reader

        chrome = Daemon._chrome_body(self, make_inner, on_event, ask_user)
        reader_on = web_reader.configured().enabled
        reader_route = Daemon._reader_route(self) if reader_on else None
        if reader_route is None:
            return chrome
        chrome_route = chrome.get("route_body")

        async def route_body(goal: str) -> str:
            try:
                body = await asyncio.to_thread(reader_route, goal)
            except Exception as e:  # noqa: BLE001 — a router that fails keeps the task on the owner's computer
                log.warning("web reader: the Jev router failed (%s)", type(e).__name__)
                body = "codex"
            if body == "web":
                return body
            return await chrome_route(goal) if chrome_route is not None else "codex"

        return {**chrome, "route_body": route_body,
                "bodies": {"web": lambda: web_reader.WebReaderAgent(make_inner, on_event, ask_user)}}

    def _reader_route(self) -> Any:
        """browser_router's Jev router, made once (None without a Jev route: every task stays where it was)."""
        if not hasattr(self, "_reader_router"):
            from . import browser_router

            self._reader_router = browser_router.jev_router()
            if self._reader_router is None:
                log.info("web reader: no Jev route (EVE_JEV_ROUTE); the web reader takes no tasks")
        return self._reader_router

    def _chrome_body(self, make_inner: Any, on_event: Any, ask_user: Any) -> dict[str, Any]:
        """The Chrome lane as ReflexFirstAgent's second body, for web goals (browser_lane.is_web_goal: a URL, a
        site, the browser). {} when the lane is off: every task stays Codex's."""
        lane = getattr(self, "_chrome_lane", None)
        if lane is None:
            return {}
        from . import browser_lane, chrome_consent, chrome_lane

        if getattr(self, "_planner_create", None) is None and not openrouter.available():
            log.warning("chrome lane: OPENROUTER_API_KEY is not set; web tasks go to Codex")
            return {}
        from .computer_agent import ComputerAgent, browser_config, make_response_creator

        telegram_on = getattr(self, "_telegram", None) is not None
        broker = chrome_consent.ConsentBroker(
            (lambda q: Daemon._ask_owner_on_phone(self, q)) if telegram_on else None,
            tell_owner=(lambda text: Daemon._tell_owner_on_phone(self, text)) if telegram_on else None,
            auto_allow=chrome_consent.access_preference() == "allow")   # EVE_CHROME_ACCESS=allow
        if getattr(self, "_planner_create", None) is None:
            self._planner_create = make_response_creator()
        create, cfg = self._planner_create, self._agent_cfg

        async def prepare(goal: str) -> None:
            await lane.connect(broker.answer_own_connection)
            await lane.use_profile(goal)

        def make_planner(ev: Any, ask: Any) -> Any:
            return ComputerAgent(create, config=browser_config(cfg), on_event=ev, ask_user=ask, browser=lane)

        async def route_body(goal: str) -> str:
            from .task_router import installed_apps

            return "chrome" if browser_lane.is_browser_only_goal(goal, installed_apps()) else "codex"

        return {"make_auto": lambda: chrome_lane.ChromeLaneAgent(make_planner, make_inner, on_event, ask_user,
                                                                  prepare=prepare, lane_screenshot=lane.screenshot,
                                                                  before_fallback=lambda: asyncio.to_thread(
                                                                      browser_lane.ensure_owner_chrome)),
                "route_body": route_body}

    async def _cancel_active_task(self, reason: str) -> None:
        """Stop a running desktop task with a reason the owner can read (log + caption),
        instead of letting a restart or a touch kill it silently (bench 2026-09-06 09:45)."""
        agent = getattr(self, "_active_agent", None)
        if agent is None or not agent.running:
            return
        log.warning("agent: cancelling the running task — %s", reason)
        agent.cancel(reason=reason)
        if self.ble.connected:
            await self.ble.send({"cmd": "caption", "lines": [f"stopped: {reason}"[:17]], "page": 0, "of": 1,
                                 "hold_ms": 4000, "final": True, "chirp": False})

    def _learning_notice(self, text: str, stage: str) -> None:
        """A lesson moved on (from the web page, `eve-bridge lesson`, or an error): the robot
        shows it. The existing voice session speaks tool results, so nothing is drawn while it is open."""
        import textwrap
        self._note_activity()
        self._learning_notices = getattr(self, "_learning_notices", 0) + 1
        if getattr(self, "bus", None) is not None:
            Daemon._publish_lesson(self, getattr(self, "_lesson_action", "") or "notice", stage, text)
        if self._conversation is not None and not self._conversation.done():
            return  # Live owns captions while the owner is talking.
        state = "error" if stage == "error" else "done" if stage == "complete" else "speaking"
        self._on_agent_state(state)
        # The robot screen holds 4 lines of 17 columns (caption_pager.PagerConfig).
        lines = textwrap.wrap(" ".join(str(text).split()), width=17)[:4] or ["..."]
        self._on_caption({"cmd": "caption", "page": 0, "of": 1, "lines": lines,
                          "hold_ms": 6000, "final": True, "chirp": state != "error"})
        # Only the latest notice may send the robot back to idle.
        handle = getattr(self, "_learning_idle_handle", None)
        if handle is not None:
            handle.cancel()
        self._learning_idle_handle = asyncio.get_running_loop().call_later(6, self._learning_idle)

    def _learning_idle(self) -> None:
        if self._conversation is None or self._conversation.done():
            self._on_agent_state("idle")

    # ---- think out loud ----

    def _make_listener(self, loop: asyncio.AbstractEventLoop) -> Any:
        """The learning server's handle on eve's microphone (LearningApp.listener).

        start() and stop() are called on the server's worker threads (an HTTP request, or run_lesson
        in asyncio.to_thread). They hand the change to the event loop, which owns every piece of
        think-aloud state, so the browser toggle, `eve-bridge lesson` and the voice tool cannot
        race each other. status() only reads."""
        daemon = self

        def call(make: Any) -> dict[str, Any]:
            try:
                on_loop = asyncio.get_running_loop() is loop
            except RuntimeError:
                on_loop = False
            if on_loop:
                raise RuntimeError("the think-out-loud listener blocks; call it from a worker thread")
            future = asyncio.run_coroutine_threadsafe(make(), loop)
            try:
                return future.result(timeout=THINK_ALOUD_CALL_SECS)
            except TimeoutError:
                future.cancel()
                return {"ok": False, "reason": "eve did not answer in time. Try again."}
            except Exception as e:  # noqa: BLE001
                log.warning("think aloud: request failed (%s)", type(e).__name__)
                return {"ok": False, "reason": "Something went wrong. Try again."}

        class Listener:
            def start(self, lesson: dict[str, Any]) -> dict[str, Any]:
                return call(lambda: daemon._think_aloud_start(lesson))

            def stop(self) -> dict[str, Any]:
                return call(daemon._think_aloud_stop)

            def status(self) -> dict[str, Any]:
                return {"state": daemon._think_aloud_state, "lesson_id": daemon._think_aloud_lesson_id}

        return Listener()

    async def _think_aloud_start(self, lesson: dict[str, Any]) -> dict[str, Any]:
        """Listen while the learner thinks out loud. Idempotent; never opens a second conversation."""
        if self._ears is None:
            return {"ok": False, "reason": "eve's microphone is off (EVE_VOICE=0, or no microphone "
                                           "was found), so eve cannot listen."}
        if not getattr(self._ears, "listening", True):
            return {"ok": False, "reason": "eve's microphone is closed because the robot is not connected. "
                                           "Plug eve in, or set EVE_MIC_ALWAYS=1."}
        if not voice_agent.live_available():
            return {"ok": False, "reason": "eve cannot listen yet: its desk voice still needs OPENAI_API_KEY in "
                                           "~/.config/eve-bridge/env (until it moves to OpenRouter)."}
        self._note_activity()
        session = self._voice_session
        talking = self._conversation is not None and not self._conversation.done()
        entered = False
        if talking and session is not None and not session.ended:
            # A conversation is open (the wake word, or listening already): it switches, no second session.
            result = await session.enter_think_aloud(lesson)
            entered = bool(result.get("ok"))
            if not entered and not session.ended:
                log.info("think aloud: start -> ok=False")
                return {"ok": False, "reason": result.get("reason") or "eve cannot listen right now."}
        if not entered and talking:
            # Still connecting (on_open applies it) or closing (_converse opens a new one after it).
            self._think_aloud_wish = lesson
            if self._think_aloud_state == "off":
                self._think_aloud_state, self._think_aloud_lesson_id = "starting", str(lesson.get("id"))
        elif not entered:
            self._start_think_aloud_conversation(lesson)
        log.info("think aloud: start -> ok=True (%s)", self._think_aloud_state)
        return {"ok": True, "answer": "I'm listening. Think out loud whenever you're ready."}

    async def _think_aloud_stop(self) -> dict[str, Any]:
        """Stop listening now: the toggle, the command, or the voice tool outside a listening session."""
        was = self._think_aloud_state
        self._think_aloud_wish = None
        session = self._voice_session
        conversation = self._conversation
        if session is not None and session.think_aloud_lesson_id is not None:
            session.stop_think_aloud("stopped")
        elif (conversation is not None and not conversation.done() and session is None
              and conversation.get_name() == THINK_ALOUD_TASK):
            conversation.cancel()               # still connecting for listening: nothing to say goodbye to
        if was != "off":
            self._on_think_aloud(False, None)
        log.info("think aloud: stop -> ok=True (was %s)", was)
        return {"ok": True, "answer": "Okay, I stopped listening." if was != "off" else "eve was not listening."}

    def _start_think_aloud_conversation(self, lesson: dict[str, Any]) -> None:
        """Open a conversation for listening, without the wake word."""
        self._think_aloud_state, self._think_aloud_lesson_id = "starting", str(lesson.get("id"))
        _spawn(self, self._dismiss_explore("think out loud"))
        self._conversation = asyncio.create_task(self._converse(think_aloud=lesson), name=THINK_ALOUD_TASK)

    def _on_voice_session(self, session: Any) -> None:
        """open_session connected: remember the session, and hand it a listen asked for while connecting."""
        self._voice_session = session
        wish, self._think_aloud_wish = getattr(self, "_think_aloud_wish", None), None
        if wish is not None:
            session.prepare_think_aloud(wish)

    def _on_think_aloud(self, on: bool, lesson_id: Optional[str]) -> None:
        """The session started or stopped listening. The web page polls this state."""
        self._think_aloud_state = "on" if on else "off"
        self._think_aloud_lesson_id = lesson_id if on else None
        log.info("think aloud: %s", "listening" if on else "off")
        Daemon._sync_listen_pose(self)

    def _start_phone_expressions(self, calls: Any, tasks: list) -> None:
        """eve's expressions in the phone's eyes (owner, 2026-10-12: "route the expressions into the eyes"): a
        picker of its own for Eve Link's voice mode, fed by the call's turns and sending only to a live call whose
        app can draw them. On unless EVE_PHONE_EXPRESSIONS=0; the board's switch file is not involved."""
        if not phone_expressions_on():
            log.info("expressions: the phone's eyes are off (EVE_PHONE_EXPRESSIONS)")
            return
        service = LiveExpressions(calls.send_expression, connected=calls.eyes_live, enabled=True, lazy=True,
                                  phase=lambda: "speaking" if getattr(calls.active, "speaking", False) else "idle")
        self._phone_expressions = service
        calls.on_text = service.offer
        tasks.append(asyncio.create_task(service.run(), name="phone-expressions"))

    def _on_expression_text(self, who: str, text: str) -> None:
        service = getattr(self, "_expressions", None)
        if service is not None:
            service.offer(who, text)

    def _on_caption(self, msg: dict) -> None:
        """One page of eve's reply (or a clear) onto the robot's screen; the pager owns the timing.
        Muted, the page goes without its talk chirp (older firmware has no sound command)."""
        if self.ble.connected:
            _spawn(self, self.ble.send(quiet_caption(msg, self._sound.muted)))

    def _on_agent_state(self, state: str) -> None:
        """Mirror the conversation/task phase on the board."""
        if state == self._agent_state:
            return
        self._agent_state = state
        self._note_activity()
        log.info("agent: %s", state)
        bus = getattr(self, "bus", None)
        if bus is not None:
            bus.publish("/eve/state", {"state": state, "time": time.time()})
        if self.ble.connected:
            _spawn(self, self.ble.send({"cmd": "agent", "state": state}))
        follower = getattr(self, "_follower", None)
        if follower is not None:
            _spawn(self, follower.on_phase(state))
        Daemon._sync_listen_pose(self)   # by class: test stubs bind only the handlers they exercise

    def _sync_listen_pose(self) -> None:
        """The board's listening pose (face the learner, solid blue "mic is live" LEDs).

        Raised while the dictation key is held, or while eve listens to a learner thinking out loud.
        The pose pins the head and overrides the phase LEDs, so it is lowered while eve thinks or
        speaks: the robot acts those out like any conversation, then shows listening again."""
        listening = getattr(self, "_think_aloud_state", "off") == "on" and getattr(
            self, "_agent_state", "idle") not in ("thinking", "speaking", "working", "error", "done")
        want = bool(getattr(self, "_listen_down", False)) or listening
        if not self.ble.connected or want == getattr(self, "_listen_sent", None):
            return
        self._listen_sent = want
        _spawn(self, self.ble.send({"cmd": "listen", "on": want}))

    # ---- idle explorer ----

    def _note_activity(self) -> None:
        """Something happened: a hook event, a board touch, the listen key."""
        self._last_activity_at = time.monotonic()

    def _idle_secs(self, now: float) -> float:
        """Seconds since the last activity. A running or waiting session is
        activity in itself, so the clock does not start until it is gone."""
        if self.state.running_count or self.state.waiting_count:
            self._last_activity_at = now
            return 0.0
        return max(0.0, now - self._last_activity_at)

    async def _explore_loop(self) -> None:
        while not self._shutdown.is_set():
            try:
                await asyncio.wait_for(self._shutdown.wait(), timeout=1.0)
                return
            except asyncio.TimeoutError:
                pass
            try:
                await self._explore_step(time.monotonic())
            except Exception:  # noqa: BLE001
                log.exception("explore: step failed")

    async def _explore_step(self, now: float) -> None:
        """One tick: gather the facts, run the schedule, execute its actions."""
        frame = None
        if self._explorer.wants_frame(now) and self._explore_raw_frame is not None:
            try:
                frame = decode_frame(self._explore_raw_frame)
            except ValueError:
                frame = None
            self._explore_raw_frame = None
        actions = self._explorer.tick(
            now,
            idle_secs=self._idle_secs(now),
            listening=bool(self._listen_sent),
            frame=frame,
            connected=self.ble.connected,
        )
        for action in actions:
            await self._run_explore_action(action)

    async def _run_explore_action(self, action: Action) -> None:
        if isinstance(action, Mode):
            if action.explore:
                log.info("explore: start (%s)", action.reason)
            else:
                log.info("explore: stop (%s)", action.reason)
                self._explore_raw_frame = None
            if self.ble.connected:
                await self.ble.send(build_mode_cmd(action.explore))
        elif isinstance(action, Look):
            log.info("explore: look yaw=%+d pitch=%d", action.yaw, action.pitch)
            if self.ble.connected:
                await self.ble.send(build_look_cmd(action.yaw, action.pitch, action.hold_ms))
        elif isinstance(action, Note):
            if self._notes is not None:
                _spawn(self, self._notes.take(action))
        elif isinstance(action, Rest):
            # Nothing over the wire: the board stays in explore mode and
            # looks around on its own until the next pan cycle.
            log.info("explore: rest (%s)", action.reason)
            self._explore_raw_frame = None

    def _show_thought(self, thought: Thought) -> None:
        """Put a thought eve just had on its own screen, if it earns it.

        The conversation owns the screen while one is open, and the listen key
        outranks anything eve has to say to itself; in either case the
        thought is skipped rather than queued, because by the time the screen
        frees up eve is looking somewhere else. Everything that gets
        past those is then judged on its own merits by thought_screen.py.
        """
        if not self._explorer.on_board or not self.ble.connected:
            return
        if self._listen_down:
            return
        if self._conversation is not None and not self._conversation.done():
            return
        now = time.monotonic()
        text = " ".join(thought.text.split())
        judged = dict(importance=thought.importance, novelty=thought.novelty,
                      cool=1.0 if thought.photographed else thought.cool)
        why = self._screen.refusal(now, text, thought.tags, **judged)
        if why is not None:
            self._screen.refused += 1
            log.info("explore: thought kept to itself (%s) — %s", why, text[:50])
            return
        self._screen.record(now, text, thought.tags)          # judged just above; offer() would judge again
        if thought.photographed:
            text = f"{text} [photo]"
        self._thought_pager.begin_reply(now)
        Daemon._on_expression_text(self, "diary", text)
        self._thought_pager.update(now, text, True)
        self._flush_thought_pager()
        log.info("explore: thought on screen — %s", text[:70])

    def _flush_thought_pager(self) -> None:
        for event in self._thought_pager.poll(time.monotonic()):
            self._on_caption(event.to_wire())

    async def _thought_caption_loop(self) -> None:
        """Turn the pages of whatever thought is on screen, and take it down
        the moment eve stops exploring."""
        while not self._shutdown.is_set():
            try:
                await asyncio.wait_for(self._shutdown.wait(), timeout=0.1)
                return
            except asyncio.TimeoutError:
                pass
            try:
                if self._thought_pager.busy and not self._explorer.on_board:
                    self._clear_thought()
                else:
                    self._flush_thought_pager()
            except Exception:  # noqa: BLE001
                log.exception("thought captions: step failed")

    def _clear_thought(self) -> None:
        for event in self._thought_pager.reset(time.monotonic()):
            self._on_caption(event.to_wire())

    async def _send_emote(self, e: Emote) -> None:
        """The diary's appraisal of what the camera saw → the board's mood engine."""
        if self.ble.connected:
            await self.ble.send(build_emote_cmd(e))

    SNAP_TIMEOUT_SECS = 3.0

    async def _take_snapshot(self) -> Optional[Frame]:
        """The diary wants a photo: ask the board for one full-size frame and
        wait for it. None when the board is away, busy with another snap, or
        silent (older firmware) — the caller then keeps the stream frame."""
        if not self.ble.connected or (self._snap_waiter is not None and not self._snap_waiter.done()):
            return None
        loop = asyncio.get_running_loop()
        self._snap_waiter = loop.create_future()
        try:
            await self.ble.send(build_snap_cmd())
            return await asyncio.wait_for(asyncio.shield(self._snap_waiter), timeout=self.SNAP_TIMEOUT_SECS)
        except asyncio.TimeoutError:
            log.info("diary: snap not answered in %.0f s (older firmware?) — keeping the stream frame",
                     self.SNAP_TIMEOUT_SECS)
            return None
        except ValueError as e:
            log.warning("diary: snap frame undecodable: %s", e)
            return None
        finally:
            self._snap_waiter = None

    async def _photo_for_owner(self, said: str) -> dict[str, Any]:
        """The voice's take_photo: one full-size frame from the board (or the
        newest streamed frame when it cannot snap), kept in the diary as a
        picture the owner asked for. The answer is what the picture shows.
        With no robot connected it is the Mac's own camera (_mac_photo)."""
        if not self.ble.connected:
            return await self._mac_photo(said)
        frame = await self._take_snapshot()
        if frame is None:
            raw = self._scene.newest_frame()
            if raw is not None:
                try:
                    frame = decode_frame(raw)
                except ValueError:
                    frame = None
        if frame is None or frame.fmt != "jpeg":
            return {"ok": False, "reason": "the camera gave no picture just now"}
        yaw, pitch = int(round(self._head.yaw)), int(round(self._head.pitch))
        if self._notes is not None:
            rec = await self._notes.keep(frame, said=said, yaw=yaw, pitch=pitch)
            if rec is None:
                return {"ok": False, "reason": "the picture could not be saved"}
            rel, caption = rec.photo, rec.caption
        else:
            # No diary model (no OPENROUTER_API_KEY for it): keep the file and a plain line.
            when = datetime.now()
            rel = photos.save(self._explore_cfg.notes_dir, when, 0, frame.data)
            if rel is None:
                return {"ok": False, "reason": "the picture could not be saved"}
            caption = said or "a picture you asked for"
            append_note(self._explore_cfg.notes_dir, when, yaw, pitch, f"A picture you asked for: {caption}",
                        extra=photos.photo_line(rel))
        self._note_activity()
        log.info("voice: photo on request -> %s", rel)
        return {"ok": True, "path": str(self._explore_cfg.notes_dir / rel), "caption": caption,
                "answer": f"Kept it: {caption}"}

    async def _mac_photo(self, said: str) -> dict[str, Any]:
        """take_photo without the robot (retired 2026-10-03): one still from the Mac's camera (mac_camera.py),
        kept on the same shelf as the robot's pictures. It is not written into the diary: the diary is the
        robot's notes, posed by its head. The caption is the owner's words; nothing looked at the picture."""
        shot = await mac_camera.snap()
        if not shot.get("ok"):
            return {"ok": False, "reason": str(shot.get("reason") or "the Mac's camera gave no picture just now")}
        rel = photos.save(self._explore_cfg.notes_dir, datetime.now(), 0, shot["jpeg"])
        if rel is None:
            return {"ok": False, "reason": "the picture could not be saved"}
        caption = said or "a picture from the Mac's camera"
        self._note_activity()
        log.info("voice: Mac camera photo on request -> %s", rel)
        return {"ok": True, "path": str(self._explore_cfg.notes_dir / rel), "caption": caption,
                "answer": f"Took it with the Mac's camera: {caption}"}

    async def _request_explore(self, reason: str) -> None:
        """The owner asked (CLI or voice): start a manual explore now.
        Raises ExploreRefused when the listen key or a disconnect stands in
        the way."""
        actions = self._explorer.request(
            time.monotonic(), reason,
            listening=bool(self._listen_sent),
            connected=self.ble.connected,
        )
        for action in actions:
            await self._run_explore_action(action)

    async def _dismiss_explore(self, reason: str) -> None:
        """End an explore now (a touch, a wake word, `explore stop`)."""
        self._clear_thought()
        for action in self._explorer.dismiss(reason):
            await self._run_explore_action(action)

    async def _stop_explore(self, reason: str) -> None:
        """Shutdown path: leave explore mode on the board if we put it there."""
        await self._dismiss_explore(reason)
        if self._notes is not None:
            self._notes.stop()

    # ---- listen key ----

    def _on_listen_key(self, on: bool) -> None:
        """Debounced edge from listen_key.py, delivered on the loop thread.
        Dedupe here, synchronously, so two fast edges cannot both read the
        same last-sent state and put duplicates on the wire."""
        log.info("listen key: %s", "down" if on else "up")
        self._listen_down = on
        self._note_activity()
        # Releasing the key while eve listens to a learner keeps the pose up.
        Daemon._sync_listen_pose(self)   # by class: test stubs bind only the handlers they exercise

    async def _reset_listen(self) -> None:
        """Board (re)connected or rebooted: tell it the key is up. Its listen
        state lives in RAM, and a reboot mid-hold would otherwise leave the
        pose stuck until the next key press."""
        await self.ble.send({"cmd": "listen", "on": False})
        self._listen_sent = False
        if getattr(self, "_think_aloud_state", "off") == "on":
            Daemon._sync_listen_pose(self)   # eve is listening to a learner: raise the pose again

    async def _update_check_loop(self) -> None:
        """Poll GitHub releases once at startup, then every 24 hours.

        Network calls happen on a thread so the asyncio loop isn't blocked by
        urllib's sync IO. Failures are logged at DEBUG and we just retry next
        cycle — never raise, never crash the daemon.
        """
        INTERVAL = 24 * 3600
        # First check is on startup (uses cache if fresh) so the hud has a
        # value to display immediately.
        first = True
        while not self._shutdown.is_set():
            info = await asyncio.to_thread(version_check, force=not first)
            self._update_available = info.latest if info.has_update else None
            if info.has_update and first:
                log.info(
                    "update available: %s → %s (you're on the latest cached info; "
                    "see https://github.com/SnowWarri0r/eve-bridge/releases)",
                    info.current, info.latest,
                )
            first = False
            try:
                await asyncio.wait_for(self._shutdown.wait(), timeout=INTERVAL)
                return
            except asyncio.TimeoutError:
                pass

    # ---- IPC handler ----

    async def _handle_lesson(self, req: dict[str, Any]) -> dict[str, Any]:
        """One lesson action over IPC, through the same run_lesson helper the voice tool uses.

        The robot acts it out: "thinking" while the tutor works, then one caption with the answer.
        While a conversation is open, Live owns the screen and nothing is drawn."""
        from .learning import TUTOR_ACTIONS, lesson_request, run_lesson
        try:
            request = lesson_request(req)
        except ValueError as e:
            return {"ok": False, "error": str(e)}
        action = request["action"]
        server = getattr(self, "_learning_server", None)
        voice = server.app.voice if server is not None else None
        talking = self._conversation is not None and not self._conversation.done()
        if action != "status":
            # The board cannot pan the room and teach at once (the wake word does the same).
            await self._dismiss_explore("lesson")
            if voice is not None and not talking and action in TUTOR_ACTIONS:
                self._on_agent_state("thinking")
        before = getattr(self, "_learning_notices", 0)
        self._lesson_action = action          # names the action in the lesson event the notice publishes
        try:
            result = await asyncio.to_thread(run_lesson, voice, request)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001
            # run_lesson lets unexpected errors (a disk error, a tutor bug) through. Treat one as a
            # refusal, so the robot never stays on "thinking". Log only the type: the message may
            # carry the learner's text.
            log.warning("lesson: %s failed (%s)", action, type(e).__name__)
            result = {"ok": False, "reason": "Something went wrong with the lesson. Try again."}
        # A notice the worker queued with call_soon_threadsafe is not certain to have run when to_thread returns
        # (3 in 40 under load, 2026-10-09: the answer's caption then showed twice). One pass of the loop runs it.
        await asyncio.sleep(0)
        # Only the action and the outcome are logged, never the learner's text, topic or answer.
        log.info("lesson: %s -> ok=%s", action, result.get("ok"))
        if not result.get("ok"):
            reason = result.get("reason") or "That did not work."
            if voice is not None:
                self._learning_notice(reason, "error")
            self._lesson_action = ""
            return {**result, "ok": False, "action": action, "reason": reason, "error": reason}
        # dispatch's notify callback is queued on the loop from the worker thread before to_thread's
        # completion, so it has run by now. open, ideas and help-mode start never notify: show those here.
        # listen and stop-listening show the listening pose instead of a caption over the new session.
        if (getattr(self, "_learning_notices", 0) == before and action != "status"
                and action not in LISTEN_ACTIONS):
            stage = (result.get("lesson") or {}).get("stage") or ""
            self._learning_notice(result.get("answer") or "", stage)
        self._lesson_action = ""
        return {**result, "ok": True, "action": action}

    async def _handle_ipc(self, req: dict[str, Any]) -> dict[str, Any]:
        evt = req.get("evt")
        # Drop pretooluse from the trace: it has its own dedicated INFO log,
        # and the volume would drown out everything else. get_state is the
        # hud polling — also too chatty to be useful here.
        if evt not in ("pretooluse", "get_state", "expressions"):
            log.info("ipc evt=%r session=%s", evt, (req.get("session_id") or "?")[:8])
        # Every hook event is activity for the idle explorer, except the
        # polls that fire on their own (statusline, diag watch).
        if evt not in ("get_state", "diag", "explore", "expressions"):
            self._note_activity()

        # One method per event (_ipc_<evt>), found in IPC_HANDLERS below the class. Called through the class,
        # not self: tests drive these handlers on a stub that carries only the methods it needs.
        handler = IPC_HANDLERS.get(evt) if isinstance(evt, str) else None
        if handler is None:
            return {"ok": False, "error": f"unknown evt: {evt!r}"}
        return await handler(self, req)

    async def _ipc_expressions(self, req: dict[str, Any]) -> dict[str, Any]:
        service = self._expressions
        action = req.get("action", "status")
        if action in ("on", "off"):
            await service.set_enabled(action == "on")
        elif action in ("react", "audition"):
            text = req.get("text")
            if not isinstance(text, str) or not text.strip() or len(text) > 2000:
                return {"ok": False, "error": "text must contain 1..2000 characters"}
            phone = getattr(self, "_phone_expressions", None)
            if (action == "react" and not self.ble.connected and phone is not None and phone.enabled
                    and phone.connected()):
                service = phone                 # no board: a react shows on the live phone call's face
            elif not service.enabled or not service.ready or not self.ble.connected:
                return {"ok": False, "error": "expressions are not ready or board is disconnected"}
            if action == "audition":
                phase = req.get("phase", "speaking")
                if phase not in ("speaking", "listening", "idle"):
                    return {"ok": False, "error": "audition phase must be speaking, listening, or idle"}
                if ((self._conversation is not None and not self._conversation.done())
                        or (self._expression_audition is not None and not self._expression_audition.done())):
                    return {"ok": False, "error": "wait until the conversation or audition ends"}
                previous = self._agent_state
                self._on_agent_state(phase)

                async def restore_phase():
                    try:
                        await asyncio.sleep(6)
                    finally:
                        if (self._agent_state == phase and
                                (self._conversation is None or self._conversation.done())):
                            self._on_agent_state(previous)

                self._expression_audition = asyncio.create_task(restore_phase(), name="expression-audition")
            event = service.offer("demo", text)
            return {"ok": event is not None, "id": event, "expressions": service.status()}
        elif action != "status":
            return {"ok": False, "error": "unknown expression action"}
        phone = getattr(self, "_phone_expressions", None)
        return {"ok": True, "connected": self.ble.connected, "expressions": service.status(),
                "phone": None if phone is None else {"connected": phone.connected(), **phone.status()}}

    async def _ipc_explore(self, req: dict[str, Any]) -> dict[str, Any]:
        # `eve-bridge explore [start|stop|status]`: the owner sends
        # the robot off to look around (or calls it back) by hand.
        action = str(req.get("action") or "start")
        if action == "start":
            try:
                await self._request_explore("requested by cli")
            except ExploreRefused as e:
                return {"ok": False, "error": str(e), "connected": self.ble.connected,
                        "explore": self._explorer.status()}
        elif action == "stop":
            self._note_activity()   # the human is here
            await self._dismiss_explore("requested by cli")
        elif action != "status":
            return {"ok": False, "error": f"unknown explore action: {action!r}"}
        return {"ok": True, "connected": self.ble.connected, "explore": self._explorer.status()}

    async def _ipc_lesson(self, req: dict[str, Any]) -> dict[str, Any]:
        # `eve-bridge lesson <action>`: the lesson voice tool's path, from a terminal.
        return await self._handle_lesson(req)

    async def _ipc_notes(self, req: dict[str, Any]) -> dict[str, Any]:
        # `eve-bridge notes start|stop|status`, and the voice path.
        action = str(req.get("action") or "status")
        # stop and status reach the phone's notes while they are on; start is always the room's
        server = getattr(getattr(self, "_miniapp", None), "server", None)
        calls = getattr(server, "calls", None)
        if action != "start" and calls is not None and getattr(calls, "notes", None) is not None:
            phone = await calls.notes_action(action, reason=str(req.get("reason") or "asked"))
            if phone is not None:
                return phone
        taker = self._room_notes_taker()
        if action == "start":
            return taker.start()
        if action == "stop":
            return await taker.stop(str(req.get("reason") or "asked"))
        return {"ok": True, **taker.status()}

    async def _ipc_trace(self, req: dict[str, Any]) -> dict[str, Any]:
        # The pose series the board reported during the last motion.
        return {"ok": True, "trace": [[round(t, 3), y, p] for t, y, p in self._motion_trace]}

    async def _ipc_pose(self, req: dict[str, Any]) -> dict[str, Any]:
        # Where the head actually is, as the BOARD reports it on every camera
        # frame (~4.3/s, head.observe). Sampling this during a motion is how a
        # delivered swing is measured without a camera pointed at the room —
        # the plan's own check, and the only one that reads the servos rather
        # than the request.
        head = self._head
        return {"ok": True, "yaw": getattr(head, "yaw", None),
                "pitch": getattr(head, "pitch", None),
                "at": getattr(head, "pose_at", None),
                "connected": self.ble.connected}

    async def _ipc_follow(self, req: dict[str, Any]) -> dict[str, Any]:
        # A follow-only test: the follower gets a conversation phase ("listening") for `secs` seconds, with no
        # voice, ring or expressions, so eve just keeps its eyes on whoever is in front of it. "stop" ends it.
        follower = self._follower
        task = getattr(self, "_follow_test", None)
        if task is not None and not task.done():
            task.cancel()
        if str(req.get("action") or "start") == "stop":
            await follower.on_phase("idle")
            if self.ble.connected:
                await self.ble.send({"cmd": "agent", "state": "idle"})
            return {"ok": True, "phase": follower.phase}
        secs = max(1.0, min(600.0, float(req.get("secs") or 60)))

        async def hold() -> None:
            try:
                await asyncio.sleep(secs)
            finally:
                if follower.phase == "listening":
                    await follower.on_phase("idle")
                    if self.ble.connected:
                        await self.ble.send({"cmd": "agent", "state": "idle"})
                    log.info("follow test: over")

        # The board accepts a host look in any state only during a conversation (hostlook.h), so the test
        # shows it one, as a desk call would; otherwise an attention state silently keeps the head.
        if self.ble.connected:
            await self.ble.send({"cmd": "agent", "state": "listening"})
        await follower.on_phase("listening")
        log.info("follow test: following for %.0f s", secs)
        self._follow_test = asyncio.create_task(hold(), name="follow-test")
        return {"ok": True, "phase": follower.phase, "secs": secs, "connected": self.ble.connected}

    async def _ipc_axis(self, req: dict[str, Any]) -> dict[str, Any]:
        # The head's axis directions (body.cpp, saved on the board). With "pitch_rev"/"yaw_rev" it sets them;
        # without, it reads them. The head calibration page (head_cal.py) drives this.
        if not self.ble.connected:
            return {"ok": False, "error": "the robot is not connected"}
        cmd: dict[str, Any] = {"cmd": "axis"}
        for k in ("pitch_rev", "yaw_rev"):
            if req.get(k) is not None:
                cmd[k] = bool(req[k])
        waiter = self.expect_ack("axis")
        try:
            await self.ble.send(cmd)
            ack = await self.wait_for_ack("axis", timeout=3.0, waiter=waiter)
        except asyncio.TimeoutError:
            return {"ok": False, "error": "the board did not answer (older firmware?)"}
        n = int(ack.get("n") or 0)
        log.info("axis: pitch_rev=%s yaw_rev=%s", bool(n & 2), bool(n & 1))
        return {"ok": True, "pitch_rev": bool(n & 2), "yaw_rev": bool(n & 1)}

    async def _ipc_move(self, req: dict[str, Any]) -> dict[str, Any]:
        # `eve-bridge move …`: a named motion the BOARD runs. The host
        # only names it; motion.h admits every number before a servo sees it,
        # so this path cannot ask for something unsafe however it is called.
        from . import motion as motion_mod
        kind = str(req.get("kind") or "osc")
        try:
            if kind == "stop":
                cmd = motion_mod.stop_command()
            elif kind == "keys":
                cmd = motion_mod.keys_command(req.get("preset"), req.get("keys"))
            else:
                cmd = motion_mod.osc_command(
                    req.get("preset"), speed=float(req.get("speed") or 1.0),
                    **{k: req[k] for k in ("yaw_amp", "pitch_amp", "period_ms",
                                           "pitch_period_ms", "phase_deg", "cycles",
                                           "dwell_pct", "jitter_pct", "center_yaw",
                                           "center_pitch") if req.get(k) is not None})
        except ValueError as e:
            return {"ok": False, "error": str(e)}
        if not self.ble.connected:
            return {"ok": False, "error": "the board is not connected"}
        ok = await self.ble.send(cmd)
        if kind != "stop":
            log.info("move: %s — %s", req.get("preset") or kind, motion_mod.describe(cmd))
        else:
            log.info("move: stop")
        return {"ok": bool(ok), "sent": cmd, "asked": motion_mod.predict(cmd),
                "connected": self.ble.connected}

    async def _ipc_celebrate(self, req: dict[str, Any]) -> dict[str, Any]:
        # Host-triggered celebration. Reuses the same `completed: true`
        # heartbeat flag the pet already celebrates on, so no firmware
        # support is needed — the board cannot tell this from a finished
        # Claude turn, which is exactly the point.
        secs = float(req.get("secs") or 5.0)
        self.state.pulse_completed(secs)
        await self._push_heartbeat(force=True)
        log.info("celebrate: pulsed for %.0fs", secs)
        return {"ok": True, "connected": self.ble.connected}

    async def _ipc_species(self, req: dict[str, Any]) -> dict[str, Any]:
        # Replaces the retired on-device menu. Firmware persists the index
        # in NVS, so this survives reboots.
        idx = int(req.get("idx") or 0)
        if self.ble.connected:
            await self.ble.send({"cmd": "species", "idx": idx})
            log.info("species: set to index %d", idx)
        return {"ok": True, "connected": self.ble.connected}

    async def _ipc_diag(self, req: dict[str, Any]) -> dict[str, Any]:
        # `eve-bridge diag`: ask the board for a fresh report, then
        # return the last one we hold. The board's reply arrives
        # asynchronously over serial, so a request now shows up in the
        # NEXT call — hence returning both.
        if self.ble.connected:
            await self.ble.send({"cmd": "diag"})
        return {"ok": True, "connected": self.ble.connected,
                "diag": self._last_diag}

    async def _ipc_identity(self, req: dict[str, Any]) -> dict[str, Any]:
        # `eve-bridge identity status|reset`. The daemon owns the
        # in-memory prints, so a reset must go through it while it runs.
        return self._handle_identity(str(req.get("action") or "status"))

    async def _ipc_session_start(self, req: dict[str, Any]) -> dict[str, Any]:
        self.state.session_start(
            req["session_id"],
            transcript_path=req.get("transcript_path"),
            cwd=req.get("cwd"),
        )
        _work_note(self, "session_start", req)
        inlet = getattr(self, "_telegram", None)
        if inlet is not None and inlet.claude:
            try:
                inlet.relay_session_started(str(req.get("cwd") or ""))
            except Exception:  # noqa: BLE001 — the hook's reply must never wait on the phone
                log.exception("telegram: relay_session_started failed")
        await self._push_heartbeat()
        return {"ok": True}

    async def _ipc_session_end(self, req: dict[str, Any]) -> dict[str, Any]:
        self.state.session_end(req["session_id"])
        _work_note(self, "session_end", req)
        await self._push_heartbeat()
        return {"ok": True}

    async def _ipc_turn_begin(self, req: dict[str, Any]) -> dict[str, Any]:
        session_id = req["session_id"]
        # A new user prompt cancels any pending deferred turn_end.
        pending = self._pending_turn_ends.pop(session_id, None)
        if pending is not None and not pending.done():
            pending.cancel()
        # Also kill any active celebrate pulse — user moved on.
        self.state.completed_until = 0.0
        self.state.session_start(session_id)  # idempotent
        self.state.turn_begin(session_id)
        _work_note(self, "turn_begin", req)
        prompt = req.get("prompt")
        if isinstance(prompt, str) and prompt:
            self.state.add_entry(f"> {truncate_utf8_bytes(prompt, _ENTRY_PAYLOAD_MAX_BYTES)}")
        # The relay's turn boundary: the lines that follow answer this prompt, read on the call it was said on only
        # if eve typed it from a call (telegram.relay_turn_began; verification/Eve/CallScope.lean).
        inlet = getattr(self, "_telegram", None)
        if inlet is not None and inlet.claude:
            sess = self.state.sessions.get(session_id)
            try:
                inlet.relay_turn_began(prompt if isinstance(prompt, str) else "", getattr(sess, "cwd", None) or "")
            except Exception:  # noqa: BLE001 — the hook's reply must never wait on the phone
                log.exception("telegram: relay_turn_began failed")
        await self._push_heartbeat()
        return {"ok": True}

    async def _ipc_turn_end(self, req: dict[str, Any]) -> dict[str, Any]:
        # Don't flip running→0 immediately; the firmware enters clock mode
        # as soon as running+waiting both hit zero, which blanks the
        # transcript HUD before the user has a chance to read the entry
        # we just added. Schedule the flip 15s out — long enough to read,
        # short enough that idle really does clock. A new turn_begin
        # cancels the scheduled task.
        session_id = req["session_id"]
        _work_note(self, "turn_end", req)       # exact, not the 15 s deferral below
        previous = self._pending_turn_ends.get(session_id)
        if previous is not None and not previous.done():
            previous.cancel()
        self._pending_turn_ends[session_id] = asyncio.create_task(
            self._deferred_turn_end(session_id, delay=15.0)
        )
        # Trigger the firmware's celebrate animation for a few seconds.
        # Set the pulse state synchronously so the heartbeat snapshot is
        # correct before anything is pushed.
        CELEBRATE_SECS = 5.0
        self.state.pulse_completed(duration_secs=CELEBRATE_SECS)
        # The phone's "typing…" for a relayed line ends with the joined session's turn.
        inlet = getattr(self, "_telegram", None)
        if inlet is not None and inlet.claude:
            sess = self.state.sessions.get(session_id)
            try:
                inlet.relay_turn_ended(getattr(sess, "cwd", None) or "")
            except Exception:  # noqa: BLE001 — the Stop hook's reply must never wait on the phone
                log.exception("telegram: relay_turn_ended failed")
        # Kick off the BLE push in the background so this coroutine can
        # return {"ok": True} immediately — the Stop hook caller must not
        # block on _push_heartbeat(force=True) or it surfaces as ETIMEDOUT
        # in the plugin's spawnSync call.
        _spawn(self, self._turn_end_side_effects(CELEBRATE_SECS))
        return {"ok": True}

    async def _ipc_pretooluse(self, req: dict[str, Any]) -> dict[str, Any]:
        _work_note(self, "pretooluse", req)      # an AskUserQuestion: the Work session waits on the owner
        return await self._handle_pretooluse(req)

    async def _ipc_permissionrequest(self, req: dict[str, Any]) -> dict[str, Any]:
        return await self._handle_permission_request(req)

    async def _ipc_push_character(self, req: dict[str, Any]) -> dict[str, Any]:
        path = req.get("path")
        if not isinstance(path, str) or not path:
            return {"ok": False, "error": "missing 'path'"}
        if not self.ble.connected:
            return {"ok": False, "error": "ble not connected"}
        try:
            from .folder_push import push_character
            result = await push_character(self, path)
        except Exception as e:  # noqa: BLE001
            log.exception("push_character failed")
            return {"ok": False, "error": f"{type(e).__name__}: {e}"}
        return {"ok": True, **result}

    async def _ipc_unpair(self, req: dict[str, Any]) -> dict[str, Any]:
        # Tell the stick to erase its stored bond so the next pairing
        # shows a fresh passkey (REFERENCE.md §Security and pairing).
        # Macos side still needs a manual 'Forget' from System Settings.
        if not self.ble.connected:
            return {"ok": False, "error": "ble not connected"}
        ok = await self.ble.send({"cmd": "unpair"})
        log.info("unpair: sent cmd:unpair to stick (ble write %s)",
                 "ok" if ok else "fail")
        return {"ok": bool(ok)}

    async def _ipc_sound(self, req: dict[str, Any]) -> dict[str, Any]:
        # `eve-bridge sound [on|off|status]`
        action = req.get("action")
        if action in ("on", "off"):
            self._set_sound(action == "on")
        return {"ok": True, "sound": "on" if self._sound.on else "off", "connected": self.ble.connected}

    async def _ipc_chat(self, req: dict[str, Any]) -> dict[str, Any]:
        """The chat window's reply box (widget/StackChanNotes/ChatView.swift): typed words, handled as a
        Telegram text from the owner, so they reach the same brain with every tool. The transcript records
        them and the replies, which is where the window reads the conversation from. The socket is 0600:
        only the owner's own programs can speak as the owner here."""
        text = str(req.get("text") or "").strip()[:CHAT_MAX_CHARS]
        tg = getattr(self, "_telegram", None)
        if tg is None or getattr(tg, "_chat_id", None) is None:
            return {"ok": False, "error": "eve's Telegram chat isn't running, or no owner has texted it yet"}
        if not text:
            return {"ok": False, "error": "nothing to send"}
        tg.hear(text, on_call=False)                     # typed, not said: never read on a call
        return {"ok": True}

    async def _ipc_mcp_tools(self, req: dict[str, Any]) -> dict[str, Any]:
        """A Work session's eve MCP server (eve_mcp.py) asks for the tools: the chat brain's own, now
        (TelegramInlet.session_tools). Only with the token of a Work session eve started that has not ended."""
        refused = _mcp_refusal(self, req)
        if refused is not None:
            return refused
        inlet = getattr(self, "_telegram", None)
        try:
            tools, _ = inlet.session_tools()
        except Exception as e:  # noqa: BLE001 — the type only: the request holds a token
            log.warning("work sessions: the tool list failed (%s)", type(e).__name__)
            return {"ok": False, "error": "eve couldn't list its tools"}
        log.info("work sessions: %s listed %d tools", str(req.get("sid"))[:8], len(tools))
        secs = float(getattr(getattr(inlet, "config", None), "ask_timeout_secs", 180.0)) + MCP_CALL_MARGIN_SECS
        return {"ok": True, "tools": tools, "call_secs": secs}

    async def _ipc_mcp_call(self, req: dict[str, Any]) -> dict[str, Any]:
        """One tool call from a Work session's eve MCP server, run through the chat's own gate and tool path
        (TelegramInlet.session_call): an acting tool asks the owner on the phone, and this waits for the answer."""
        refused = _mcp_refusal(self, req)
        if refused is not None:
            return refused
        inlet = getattr(self, "_telegram", None)
        name, args = req.get("name"), req.get("args")
        # The daemon bounds the call, never the MCP side: it answers within MCP_CALL_BOUND_SECS of the ask timeout,
        # before eve_mcp.py gives up waiting. A call still running then is not cancelled (an owner-approved
        # purchase or send may be half done); it goes on in _background and the session is told the outcome is
        # unknown and not to retry it.
        ask = float(getattr(getattr(inlet, "config", None), "ask_timeout_secs", 180.0))
        task = asyncio.ensure_future(inlet.session_call(name, args if isinstance(args, dict) else {},
                                                        sid=str(req.get("sid"))))
        try:
            result, outside = await asyncio.wait_for(asyncio.shield(task), timeout=ask + MCP_CALL_BOUND_SECS)
        except asyncio.TimeoutError:
            background = getattr(self, "_background", None)
            if isinstance(background, set):
                background.add(task)
                task.add_done_callback(background.discard)
            task.add_done_callback(_mcp_late_done)
            log.warning("work sessions: %s's tool call is still running after %.0f s; told the session the "
                        "outcome is unknown", str(req.get("sid"))[:8], ask + MCP_CALL_BOUND_SECS)
            return {"ok": False, "error": MCP_STILL_RUNNING}
        except asyncio.CancelledError:
            task.cancel()
            raise
        except Exception as e:  # noqa: BLE001 — the type only: the request holds a token and the arguments
            log.warning("work sessions: a tool call failed (%s)", type(e).__name__)
            return {"ok": False, "error": "that tool call failed in eve"}
        return {"ok": True, "result": result, "outside": outside}

    async def _ipc_mic(self, req: dict[str, Any]) -> dict[str, Any]:
        # `eve-bridge mic [on|off|status]`, and the menu-bar app's switch.
        action = req.get("action")
        if action in ("on", "off"):
            if self._mic.set(action == "on"):
                log.info("mic: %s (owner's choice)", action)
            self._apply_mic("your choice")
        return self._mic_status()

    async def _ipc_get_state(self, req: dict[str, Any]) -> dict[str, Any]:
        # Queried by the `eve-bridge hud` subcommand (or anyone else
        # who wants a one-shot snapshot). Kept small on purpose.
        return {
            "ok": True,
            "state": {
                "ble_connected": self.ble.connected,
                "mic_listening": self._ears is not None and self._ears.listening,
                "sec": self._last_stick_sec,
                "battery_pct": self._last_stick_battery_pct,
                "total": self.state.total,
                "running": self.state.running_count,
                "waiting": self.state.waiting_count,
                "tokens_cumulative": self.state.tokens_cumulative,
                "tokens_today": self.state.tokens_today,
                "cost_cumulative": self.state.cost_cumulative,
                "cost_today": self.state.cost_today,
                "update_available": self._update_available,
                "last_entry": self.state.entries[0].text if self.state.entries else "",
            },
        }

    async def _ipc_posttooluse(self, req: dict[str, Any]) -> dict[str, Any]:
        # A tool ran → any terminal-side permission prompt was answered.
        self.state.input_received(req.get("session_id", ""))
        _work_note(self, "posttooluse", req)
        tool_name = req.get("tool_name")
        if isinstance(tool_name, str):
            self.state.add_entry(f"+ {tool_name}")
            self._ensure_session(req)
            self.state.note_tool(req.get("session_id", ""), tool_name)
        await self._push_heartbeat()
        return {"ok": True}

    async def _ipc_notification(self, req: dict[str, Any]) -> dict[str, Any]:
        # Only a session blocked on the user (a permission prompt, a question
        # dialog) marks the session, so heartbeats carry waiting>0 — the
        # firmware's attention animation + LED pulse. An idle reminder is
        # news, not a request: it must not take over a live conversation.
        kind = req.get("notification_type")
        kind = kind if isinstance(kind, str) else None
        waits = notification_waits(kind)
        if waits:
            self.state.needs_input(req.get("session_id", ""))
        _work_note(self, "notification", req)
        msg = req.get("message")
        if isinstance(msg, str) and msg.strip():
            self.state.add_entry(f"! {msg.strip()}")
        log.info("notification: session=%s type=%s → %s",
                 (req.get("session_id") or "?")[:8],
                 kind or "?", "attention" if waits else "not waiting")
        await self._push_heartbeat()
        inlet = getattr(self, "_telegram", None)
        if inlet is not None and inlet.claude:
            _spawn(self, inlet.relay_notification(kind or "", msg if isinstance(msg, str) else "", waits))
        return {"ok": True}

    async def _handle_permission_request(self, req: dict[str, Any]) -> dict[str, Any]:
        """A permission dialog Claude Code is about to show (hooks/permission_request.py). With the relay on,
        the owner is on the phone and nobody is at the dialog: ask there, yes/no. No answer, relay off, or a
        question dialog (AskUserQuestion is answered by typing the option, never by a hook): no decision."""
        inlet = getattr(self, "_telegram", None)
        tool_name = str(req.get("tool_name") or "tool")
        if inlet is None or not inlet.claude or tool_name == "AskUserQuestion":
            return {"ok": True}
        hint = str(req.get("hint") or "")
        decision = await inlet.decide_permission(tool_name, hint, str(req.get("cwd") or ""), always=True)
        log.info("permissionrequest for %s (%s): %s", tool_name, hint[:60],
                 f"answered from Telegram → {decision}" if decision in ("allow", "deny") else "no answer → the dialog")
        if decision in ("allow", "deny"):
            self.audit.record(session_id=req.get("session_id") or "unknown", tool_name=tool_name, hint=hint,
                              matcher="permission_request", decision=decision, source="telegram")
            return {"ok": True, "decision": decision}
        return {"ok": True}

    async def _handle_pretooluse(self, req: dict[str, Any]) -> dict[str, Any]:
        tool_use_id = req.get("tool_use_id")
        if not isinstance(tool_use_id, str) or not tool_use_id:
            return {"ok": False, "error": "missing tool_use_id"}
        session_id = req.get("session_id") or "unknown"
        tool_name = req.get("tool_name") or "tool"
        hint = req.get("hint") or ""
        self._ensure_session(req)
        self.state.note_tool(session_id, tool_name)

        # A question for the owner: relayed with its numbered options, and nothing else. It is answered by
        # typing the option into the dialog, never by a decision here — and its text must not meet the
        # command matchers below (a question that mentions "rm" is not an rm).
        if tool_name == "AskUserQuestion":
            relay = getattr(self, "_telegram", None)
            if relay is not None and relay.claude:
                relay.relay_tool_call(str(tool_name), hint)
            return {"ok": True}

        # Read tool takes its own path: in-cwd reads never prompt; out-of-cwd reads
        # are allowed by the relay, else Claude Code's own flow. See read_policy.py.
        if tool_name == "Read":
            return await self._handle_read_pretooluse(req, tool_use_id, session_id, hint)

        # Smart matcher: classify trivial / risky commands first.
        # auto_allow → approve immediately, no prompt anywhere (keeps ls/cat fast).
        # always_ask → asked on the phone when the relay is on, else Claude Code's flow.
        # default    → no decision, let Claude Code's native permission flow run.
        decision_class = classify_command(hint, self.matchers)
        audit_kwargs = dict(
            session_id=session_id, tool_name=tool_name, hint=hint, matcher=decision_class,
        )
        relay = getattr(self, "_telegram", None)
        if relay is not None and relay.claude:
            relay.relay_tool_call(str(tool_name), hint)          # only a question for the owner leaves; calls stay gray
        if decision_class == "allow":
            log.info("pretooluse for %s (%s): auto_allow match → allow", tool_name, hint[:60])
            self.audit.record(**audit_kwargs, decision="allow", source="auto_allow")
            return {"ok": True, "decision": "allow"}

        # "claude on" (telegram.py): the owner is on the phone, so a prompt on the Mac has nobody at it.
        # The relay is bypass (owner, 2026-09-21): a call is allowed here without asking, as
        # bypassPermissions would. The owner's own always_ask list (rm, sudo: matchers.py) is the one
        # exception, asked in the chat as a yes/no that only the owner's next message answers; silence
        # there defers to Claude Code's own flow, never denies. EVE_TELEGRAM_ASK=1 asks every call.
        # Decided before the robot check: the phone is for when the owner is away.
        inlet = getattr(self, "_telegram", None)
        mode = str(req.get("permission_mode") or "")
        cwd = str(req.get("cwd") or "")
        # The Auto Mode gate (typed_ask.py, the Jev Engineering article): a Bash command the regex list did
        # not stop is judged by Jev before it runs, in EVERY permission mode, because bypass is exactly when
        # nothing else would stop it. "shadow" only logs the verdict; "ask" turns a risky one into the
        # phone's yes/no; "off" is the relay of 2026-09-21. A safe verdict or an error changes nothing.
        risk = self._command_risk() if inlet is not None and inlet.claude and tool_name == "Bash" and hint else None
        if risk is not None and decision_class != "ask":
            risk_mode, ask_cmd = risk
            if risk_mode == "shadow":
                _spawn(self, self._shadow_command_risk(ask_cmd, tool_name, hint, cwd, audit_kwargs),
                       name="command-risk-shadow")
            elif risk_mode == "ask":
                verdict = await asyncio.to_thread(ask_cmd, tool_name, hint, cwd)
                jev = {"verdict": verdict.decision, "why": verdict.why, **{k: round(v, 3) for k, v in
                       verdict.answer.nouls().items()}, "ms": round(verdict.answer.ms)}
                if verdict.decision == "risky":
                    log.info("pretooluse for %s (%s): Jev says risky (%s) → asking the phone", tool_name,
                             hint[:60], verdict.why)
                    decision = await inlet.decide_permission(tool_name, f"{hint} [Jev: {verdict.why}]", cwd,
                                                             always=True)
                    if decision in ("allow", "deny"):
                        self.audit.record(**audit_kwargs, decision=decision, source="telegram", jev=jev)
                        return {"ok": True, "decision": decision}
                    log.info("pretooluse for %s (%s): no answer from Telegram → defer", tool_name, hint[:60])
                    self.audit.record(**audit_kwargs, decision=None, source="jev_risky_deferred", jev=jev)
                    return {"ok": True}
                source = "jev_safe" if verdict.decision == "safe" else "jev_error"
                if verdict.decision != "safe":
                    log.warning("pretooluse for %s: the risk model failed (%s); allowed as before", tool_name,
                                verdict.why)
                if mode in ("bypassPermissions", "dontAsk"):
                    self.audit.record(**audit_kwargs, decision=None, source=source, jev=jev)
                    return {"ok": True}                       # bypass: Claude Code allows on its own
                self.audit.record(**audit_kwargs, decision="allow", source=source, jev=jev)
                return {"ok": True, "decision": "allow"}
        if inlet is not None and inlet.claude and mode not in ("bypassPermissions", "dontAsk"):
            asks_all = bool(getattr(getattr(inlet, "config", None), "ask_permissions", False))
            if decision_class == "ask" or asks_all:
                decision = await inlet.decide_permission(tool_name, hint, cwd, always=True)
                if decision in ("allow", "deny"):
                    log.info("pretooluse for %s (%s): answered from Telegram → %s", tool_name, hint[:60], decision)
                    self.audit.record(**audit_kwargs, decision=decision, source="telegram")
                    return {"ok": True, "decision": decision}
                log.info("pretooluse for %s (%s): no answer from Telegram → defer", tool_name, hint[:60])
            else:
                log.info("pretooluse for %s (%s): claude relay on → allow", tool_name, hint[:60])
                self.audit.record(**audit_kwargs, decision="allow", source="telegram_relay")
                return {"ok": True, "decision": "allow"}

        # If BLE isn't connected, skip the round-trip and return no decision so
        # Claude Code's normal flow runs (respects user's auto/allow settings).
        if not self.ble.connected:
            # "stick", not "ble": self.ble duck-types over serial too, and the
            # 2026-08-06 log read as a BLE problem when the board was simply
            # off the USB bus.
            log.info("pretooluse for %s: stick not connected, deferring to default flow", tool_name)
            self.audit.record(**audit_kwargs, decision=None, source="ble_disconnected")
            return {"ok": True}

        # The board has no decision surface any more — the swipe card is
        # gone, so nothing can answer a prompt. Always defer to Claude Code's
        # own permission flow rather than block the tool call waiting on a
        # button that no longer exists. Placed after auto_allow so the matcher
        # fast paths keep working.
        log.info("pretooluse for %s (%s): no card → defer to default", tool_name, hint[:60])
        self.audit.record(**audit_kwargs, decision=None, source="defer")
        return {"ok": True}

    def _command_risk(self) -> Optional[tuple[str, Any]]:
        """(mode, asker) for the Auto Mode gate, or None when it is off or cannot be built. Built once.
        EVE_COMMAND_RISK is off | shadow | ask (typed_ask.COMMAND_RISK_DEFAULT when unset); a route
        without a key is "off" with one log line, never an error on the hook path."""
        cached = getattr(self, "_command_risk_cache", None)
        if cached is not None:
            return cached or None
        from . import typed_ask
        raw = (os.environ.get("EVE_COMMAND_RISK") or typed_ask.COMMAND_RISK_DEFAULT).strip().lower()
        if raw not in typed_ask.COMMAND_RISK_MODES:
            log.warning("command risk: EVE_COMMAND_RISK=%r is not one of %s; off", raw, typed_ask.COMMAND_RISK_MODES)
            raw = "off"
        built: Any = False
        if raw != "off":
            try:
                from . import jev
                url, key, model = jev.route_config(os.environ)
                seconds = jev.timeout_from_env(os.environ)
                asker = typed_ask.make_jev_command_asker(jev.make_predict(url, key, model, timeout_s=seconds),
                                                         time.perf_counter)
                built = (raw, asker)
                log.info("command risk: %s (Jev %s judges a relayed Bash command: destroys, escapes, publishes, "
                         "secrets; obvious secrets redacted first)", raw, model)
            except Exception as e:  # noqa: BLE001
                log.warning("command risk: off (%s: %s)", type(e).__name__, e)
        else:
            log.info("command risk: off (the relay allows what the regex list does not stop)")
        self._command_risk_cache = built
        return built or None

    async def _shadow_command_risk(self, ask_cmd: Any, tool_name: str, hint: str, cwd: str,
                                   audit_kwargs: dict[str, Any]) -> None:
        """Shadow mode: the verdict is logged beside the regex class and never acted on."""
        try:
            verdict = await asyncio.to_thread(ask_cmd, tool_name, hint, cwd)
        except Exception as e:  # noqa: BLE001 — shadow: nothing may reach the hook path
            log.warning("command risk (shadow): %s", type(e).__name__)
            return
        self.audit.record(**audit_kwargs, decision=None, source="jev_shadow",
                          jev={"verdict": verdict.decision, "why": verdict.why,
                               **{k: round(v, 3) for k, v in verdict.answer.nouls().items()},
                               "ms": round(verdict.answer.ms)})

    def _ensure_session(self, req: dict[str, Any]) -> None:
        """Register the session behind a hook event if we've never seen it.

        SessionStart is the only hook that normally creates a session, so a
        daemon restart leaves every *already-running* session invisible until
        the human restarts it too — the monitor then shows an empty agent list
        while agents are plainly working. Tool hooks carry both ids we need,
        and session_start is create-if-absent, so adopting the session here is
        free and makes the list self-heal within one tool call.
        """
        session_id = req.get("session_id")
        if not (isinstance(session_id, str) and session_id):
            return
        cwd = req.get("cwd") or None
        self.state.session_start(session_id, cwd=cwd)
        # session_start is create-if-absent, so it will NOT fill in a cwd it
        # missed. posttooluse doesn't carry one, so whichever hook happens to
        # fire first decides the row's name forever — and losing the race
        # leaves the agent labelled with a session-id prefix instead of its
        # repo. Backfill the first cwd we actually see.
        sess = self.state.sessions.get(session_id)
        if sess is not None and cwd and not sess.cwd:
            sess.cwd = cwd

    async def _handle_read_pretooluse(
        self, req: dict[str, Any], tool_use_id: str, session_id: str, hint: str,
    ) -> dict[str, Any]:
        """Read-tool prompts: card out-of-cwd reads; approval grants the whole
        enclosing scope (git repo or parent dir) for the daemon's lifetime.
        See read_policy.py for why scope-not-file is the point."""
        path = hint
        cwd = req.get("cwd") or ""
        audit_kwargs = dict(
            session_id=session_id, tool_name="Read", hint=path, matcher="read",
        )
        # In-cwd reads never prompt anywhere; defer without noise.
        if not path or is_within(path, cwd):
            return {"ok": True}
        inlet = getattr(self, "_telegram", None)
        if inlet is not None and inlet.claude and str(req.get("permission_mode") or "") not in ("bypassPermissions", "dontAsk"):
            # The Claude relay is bypass (see _handle_pretooluse): a read is allowed, not carded.
            log.info("read %s: claude relay on → allow", path)
            self.audit.record(**audit_kwargs, decision="allow", source="telegram_relay")
            return {"ok": True, "decision": "allow"}
        if not self.ble.connected:
            self.audit.record(**audit_kwargs, decision=None, source="ble_disconnected")
            return {"ok": True}
        # No card to show it on — defer to Claude Code's own flow.
        self.audit.record(**audit_kwargs, decision=None, source="defer")
        return {"ok": True}

    # ---- BLE handler ----

    async def _handle_ble(self, obj: dict[str, Any]) -> None:
        frame = obj.get("frame")
        if isinstance(frame, dict):
            if frame.get("snap") is True:
                # The photo we asked for: hand it to the waiting diary, never
                # to the face tracker (it is 320x240, and it is not a stream
                # frame). An unasked-for snap is dropped.
                waiter, self._snap_waiter = self._snap_waiter, None
                if waiter is not None and not waiter.done():
                    try:
                        waiter.set_result(decode_frame(frame))
                    except ValueError as e:
                        waiter.set_exception(e)
                return
            # Camera frame: hottest object on the link (5/s). Hand it to the
            # tracker, which parks or runs it and replies with the face cmd.
            # While exploring, also keep the newest one for the next waypoint
            # sample (the explore loop decodes it, once, when it is due).
            if self._explorer.active:
                self._explore_raw_frame = frame
            # The head pose rides on every frame; the scene watcher keeps only the
            # newest frame, and only while a conversation is open.
            self._head.observe(frame.get("yaw"), frame.get("pitch"))
            self._scene.offer(frame)
            await self._vision.on_frame(frame)
            return
        expression = obj.get("expression")
        if isinstance(expression, dict):
            service = getattr(self, "_expressions", None)
            if service is not None:
                service.observe(expression)
            return
        diag = obj.get("diag")
        if isinstance(diag, dict):
            # Crash/hang report from the board (see firmware diag.h). Logged at
            # WARNING when the previous run died abnormally, because that line
            # is the whole point: it says what the board was doing when it
            # froze, which nothing else on this link can tell us.
            reset = diag.get("reset", "?")
            abnormal = reset in ("PANIC", "TASK-WATCHDOG", "INT-WATCHDOG",
                                 "BROWNOUT", "other-watchdog")
            self._last_diag = diag
            log.log(
                logging.WARNING if abnormal else logging.INFO,
                "board diag: boot #%s after %s (up=%ss heap=%s min=%s psram=%s)",
                diag.get("boot"), reset, diag.get("up"),
                diag.get("heap"), diag.get("minheap"), diag.get("psram"),
            )
            # The single most valuable field after a watchdog reset: which
            # loop() phase never returned. The firmware has always sent it;
            # dropping it here is why a week of TASK-WATCHDOG resets never
            # named the hanging call.
            if "diedIn" in diag:
                log.log(
                    logging.WARNING if abnormal else logging.INFO,
                    "  died in: %s (entered %sms, %s loops)",
                    diag.get("diedIn"), diag.get("diedAtMs"), diag.get("loops"),
                )
            for ev in diag.get("last") or []:
                log.log(logging.WARNING if abnormal else logging.INFO,
                        "  pre-reset event: %s", ev)
            return
        cmd = obj.get("cmd")
        if cmd == "explore":
            # Two quick taps on the screen: come back. The board only sends
            # this while it is exploring, so it always means stop.
            self._note_activity()
            log.info("explore: called back by a double tap")
            await self._dismiss_explore("double tap")
            return
        if cmd == "music":
            # Spotify mode's input from the Voice PE (music_mode.py): clicks, holds and the dial.
            self._note_activity()
            mode = getattr(self, "_music_mode", None)
            if mode is not None:
                await mode.on_input(obj)
            else:
                ctl = getattr(self, "_controller", None)
                if ctl is not None:                        # no Spotify: a board stuck in the mode is put back
                    await ctl.show({"cmd": "music_mode", "on": False})
            return
        if cmd == "ptt":
            # The board's button held to talk (Voice PE, desk_call.py): the Mini App's push-to-talk call, on
            # the desk. A wake-word conversation already has the mic, so a press during one is ignored. A
            # release always goes through: dropped, it left the mic of a press made before the conversation
            # open (verification/Eve/DeskCall.lean).
            self._note_activity()
            on = bool(obj.get("on"))
            self._ptt_held = on
            if on and self._conversation is not None and not self._conversation.done():
                log.info("board ptt during a conversation — ignored")
                return
            await self._desk_calls_get().press(on)
            return
        if cmd in ("focus", "key"):
            # A touch on the board: the human is here, stop exploring — at
            # once, and even a manual explore, which ignores the idle clock.
            self._note_activity()
            await self._dismiss_explore(f"touch ({cmd})")
            # ... but a touch during a conversation does nothing more. On
            # 2026-09-10 a phantom body-pad blip in the attention pose sent
            # focus and killed a live task; a terminal raise would also take
            # the frontmost app from a running computer-use task. The
            # conversation ends by voice (goodbye), not by a touch.
            if self._conversation is not None and not self._conversation.done():
                log.info("board touch (%s) during a conversation — ignored", cmd)
                return
        if cmd == "focus":
            # A tap on the pet in attention state: raise the terminal of the
            # session that is waiting on the human (oldest pending permission,
            # else newest needs-input session).
            from .focus_terminal import focus_session_terminal
            cwd = self.state.attention_cwd()
            log.info("focus: requested for %r", cwd or "(unknown session)")
            _spawn(self, focus_session_terminal(cwd))
            return
        if cmd == "key":
            # Swipe-down on the pet → Enter on the host.
            if self._keys is None:
                from .key_tap import KeyTapper
                self._keys = KeyTapper()
            self._keys.tap(str(obj.get("name") or ""))
            return
        # {"pose":{"y":tenths,"p":tenths}}: where the head actually is, ten times a
        # second, but only while a motion runs. It exists because the camera
        # frames that normally carry the pose are quarantined for the whole of a
        # movement, so this is the only way to measure a delivered swing.
        pose = obj.get("pose")
        if isinstance(pose, dict):
            yaw, pitch = pose.get("y"), pose.get("p")
            if isinstance(yaw, (int, float)) and isinstance(pitch, (int, float)):
                self._head.observe(yaw / 10.0, pitch / 10.0)
                self._motion_trace.append((time.monotonic(), yaw / 10.0, pitch / 10.0))
                del self._motion_trace[:-400]
            return

        # Status acks come back from the device after we poll with {"cmd":"status"}.
        # Shape per REFERENCE.md: {"ack":"status","ok":true,"data":{"name","sec","bat":{...},"sys":{...},"stats":{...}}}.
        ack = obj.get("ack")
        if ack == "status":
            # A status reply proves the write path works RIGHT NOW — but the
            # observed wedge pattern is flapping: a fresh reopen restores TX
            # for one exchange (63-183s) and then goes deaf again. Zeroing the
            # escalation on any single ack meant pulse_reset could never fire.
            # Only a streak of clean poll cycles (counted in the poller)
            # de-escalates; here we just mark the poll answered.
            #
            # Only an ack that answers an OUTSTANDING watchdog poll counts.
            # The {"cmd":"status"} in _send_resync goes out on every reconnect
            # and is answered by the same fresh link that then goes deaf; when
            # it counted, "resync ack + one poll ack" made the streak of two
            # after every reconnect and the RTS pulse never fired
            # (verification/Eve/Serial.lean, B).
            answered_poll = self._status_sent_at is not None
            self._status_sent_at = None
            self._status_missed = 0
            if answered_poll:
                self._clean_polls += 1
                if self._ack_escalation and self._clean_polls >= 2:
                    log.info("status acks stable for %d clean polls — "
                             "resetting ack escalation (was %d)",
                             self._clean_polls, self._ack_escalation)
                    self._ack_escalation = 0
        if ack == "status" and obj.get("ok"):
            data = obj.get("data") or {}
            # "sec" is the board's BLE encryption; over a USB cable there is nothing to sniff,
            # so leave it unknown (no warning, no HUD "UNSEC").
            sec = None if getattr(self.ble, "wired", False) else data.get("sec")
            if sec is not None and sec != self._last_stick_sec:
                log.info(
                    "stick link: %s (was %s)",
                    "ENCRYPTED" if sec else "UNENCRYPTED — transcript sniffable!",
                    self._last_stick_sec,
                )
                self._last_stick_sec = bool(sec)
            snd = data.get("snd")               # firmware with {"cmd":"sound"}; older boards omit it
            if isinstance(snd, bool) and snd != self._sound.on:
                log.info("sound: board has sound %s but the owner chose %s — re-sending",
                         "on" if snd else "off", "on" if self._sound.on else "off")
                _spawn(self, self._send_sound())
            bat = data.get("bat") or {}
            if isinstance(bat, dict) and bat:
                pct = bat.get("pct")
                ma = bat.get("mA")
                if isinstance(pct, int) and pct != self._last_stick_battery_pct:
                    charging = "+" if isinstance(ma, int) and ma < 0 else " "
                    log.info("stick battery: %d%% %s", pct, charging)
                    self._last_stick_battery_pct = pct
            sys_info = data.get("sys") or {}
            if isinstance(sys_info, dict):
                free = sys_info.get("fsFree")
                total = sys_info.get("fsTotal")
                if isinstance(free, int) and isinstance(total, int):
                    if total == 0:
                        # LittleFS isn't mounted. Firmware calls begin(false),
                        # so an un-formatted partition reports 0/0. push-character
                        # will fail with "have 0K" until the user factory-resets
                        # the stick (hold A → settings → reset → factory reset),
                        # which runs LittleFS.format().
                        log.error(
                            "stick LittleFS appears unformatted (fsTotal=0). "
                            "Run factory reset on the stick to format it; "
                            "push-character will reject until then."
                        )
                    else:
                        log.info("stick fs: %d/%d bytes free (%.0f%%)",
                                 free, total, 100.0 * free / total)
            return

        # Route any ack (status already handled above, but others — char_begin,
        # file, chunk, file_end, char_end, name, owner, unpair, etc.) to the
        # oldest waiter that registered for that ack type. A waiter that names
        # an "n" takes only a successful ack carrying that n: a late ack from a
        # timed-out request must not answer the next one (firmware failure acks
        # carry n=0, so those still match).
        if ack is not None:
            for waiter_type, want_n, fut in self._ack_waiters:
                if waiter_type != ack or fut.done():
                    continue
                if want_n is not None and obj.get("ok") and obj.get("n") != want_n:
                    log.debug("ack %s n=%r is stale (waiting for n=%r) — ignored",
                              ack, obj.get("n"), want_n)
                    continue
                fut.set_result(obj)
                break
            return

        if cmd in {"name", "owner", "unpair", "char_begin", "char_end", "file", "file_end", "chunk"}:
            # We're the central; we don't send these, but acknowledge defensively.
            return

        log.debug("ble: unhandled %r", obj)

    # ---- JSONL callback ----

    async def _on_tokens(
        self,
        cumulative: int,
        today: int,
        cost_cumulative: float,
        cost_today: float,
        _entries: list,
    ) -> None:
        self.state.set_tokens(cumulative, today, cost_cumulative, cost_today)
        await self._push_heartbeat()

    async def _on_assistant_text(self, _transcript_path: str, text: str, _uuid: str) -> None:
        """Fired by the JSONL tailer the moment a new assistant text record
        lands on disk (typically <500 ms after Claude Code finishes the
        message). Emitting here beats the Stop hook, so the stick receives
        the '@ ...' entry while the user is still looking at the terminal —
        before auto-off kicks in.

        Only a session eve's hooks know: the tailer reads every transcript
        under the roots, and a background Claude (claude-mem's observer,
        whose hooks never reach eve) once had its "Prompt is too long" error
        relayed to the phone as Claude's answer (2026-10-12). Claude Code
        names a transcript <session_id>.jsonl."""
        session = next((s for s in self.state.sessions.values()
                        if s.transcript_path == _transcript_path), None) \
            or self.state.sessions.get(os.path.splitext(os.path.basename(_transcript_path))[0])
        if session is None:
            log.debug("tailer: assistant text from an unknown session, not relayed")
            return
        self.state.add_entry(f"@ {truncate_utf8_bytes(text, _ENTRY_PAYLOAD_MAX_BYTES)}")
        log.info("tailer: new assistant text → entry added (state.entries=%d)",
                 len(self.state.entries))
        await self._push_heartbeat(force=True)
        inlet = getattr(self, "_telegram", None)
        if inlet is not None and inlet.claude:
            _spawn(self, inlet.relay_text(text, session.cwd or ""))

    async def _turn_end_side_effects(self, celebrate_secs: float) -> None:
        """Post-response work for a turn_end event, run as a background task.

        Runs *after* the IPC ``{"ok": True}`` reply has been sent, so the Stop
        hook's spawnSync call never waits on a BLE write.

        1. Force-push the heartbeat carrying ``completed=true`` so the stick's
           celebrate animation fires within ~50 ms of the turn ending.
        2. Schedule a follow-up push at pulse end so ``completed`` flips back
           to false on time rather than waiting for the next keepalive.
        """
        try:
            await self._push_heartbeat(force=True)
        except Exception:  # noqa: BLE001
            log.exception("turn_end side effects: _push_heartbeat(force=True) failed")
        _spawn(self, self._heartbeat_after(celebrate_secs + 0.1))

    async def _heartbeat_after(self, delay: float) -> None:
        """Schedule one heartbeat push after ``delay`` seconds. Used by the
        celebrate-pulse logic to flush the completed=false transition right
        when the pulse expires, instead of waiting for the next keepalive."""
        try:
            await asyncio.sleep(delay)
            await self._push_heartbeat(force=True)
        except asyncio.CancelledError:
            return

    async def _deferred_turn_end(self, session_id: str, delay: float) -> None:
        """Delay the state.turn_end so that running>0 keeps the firmware out of
        clock mode long enough to render the @-entry the tailer just pushed."""
        try:
            await asyncio.sleep(delay)
            self.state.turn_end(session_id)
            await self._push_heartbeat()
        except asyncio.CancelledError:
            return
        finally:
            # Self-cleanup. If a new turn_begin already replaced the task, the
            # pop returns that task instead — ignore the mismatch.
            current = self._pending_turn_ends.get(session_id)
            if current is not None and current.done():
                self._pending_turn_ends.pop(session_id, None)

    # ---- turn event ----

    def expect_ack(self, ack_type: str, n: Optional[int] = None) -> asyncio.Future[dict[str, Any]]:
        """Register a waiter for ``ack_type`` NOW, before the request goes out,
        and return its future; pass it to wait_for_ack(waiter=...). Registering
        after the send left a window in which the board's ack reached the
        router with no waiter and was dropped (verification/Eve/Serial.lean,
        C). ``n``, when given, is the ack's expected "n" field."""
        # Waiters whose request never went out (the send failed) were cancelled
        # by their caller; prune them here.
        self._ack_waiters = [w for w in self._ack_waiters if not w[2].done()]
        fut: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
        self._ack_waiters.append((ack_type, n, fut))
        return fut

    async def wait_for_ack(self, ack_type: str, timeout: float = 5.0, *,
                           waiter: Optional[asyncio.Future[dict[str, Any]]] = None) -> dict[str, Any]:
        """Block until we receive an ack matching ``ack_type``. Used by the
        folder-push flow — the firmware requires a per-chunk ack before we
        send the next chunk, since its UART RX buffer is only ~256 bytes.
        Pass the ``waiter`` from expect_ack to wait on one registered before
        the send; without it the waiter is registered here."""
        fut = waiter if waiter is not None else self.expect_ack(ack_type)
        try:
            return await asyncio.wait_for(fut, timeout=timeout)
        finally:
            self._ack_waiters = [w for w in self._ack_waiters if w[2] is not fut]


CHAT_MAX_CHARS = 4000     # a Telegram message is at most 4096 characters
MCP_CALL_MARGIN_SECS = 120.0   # an eve MCP call waits the chat's ask_timeout_secs plus this (eve_mcp.py)
MCP_CALL_BOUND_SECS = 90.0     # the daemon answers within ask_timeout_secs plus this: before the MCP side gives up
MCP_STILL_RUNNING = ("that call is still running in eve, so its outcome is unknown: it may yet run or finish. "
                     "Do not retry it; ask the owner what happened.")


def _mcp_late_done(task: "asyncio.Future[Any]") -> None:
    """A Work session's tool call that outlived its answer: log how it ended (never its result)."""
    if task.cancelled():
        log.info("work sessions: a late tool call was cancelled")
        return
    err = task.exception()
    log.info("work sessions: a late tool call finished (%s)", type(err).__name__ if err else "done")


def _spawn(daemon: Any, coro: Any, name: Optional[str] = None) -> "asyncio.Task[Any]":
    """Fire-and-forget work, held in the daemon's `_background` set. The event loop keeps only a weak
    reference to a task, so one nobody holds can be collected mid-run; held here it also gets drained at
    shutdown (_drain_background) rather than cancelled. A stub daemon in a test may have no set: the task
    still runs."""
    task = asyncio.create_task(coro, name=name)
    background = getattr(daemon, "_background", None)
    if isinstance(background, set):
        background.add(task)
        task.add_done_callback(background.discard)
    return task


def _work_note(daemon: Any, evt: str, req: dict[str, Any]) -> None:
    """A hook event for the Work sessions' registry (work_sessions.WorkSessions.note), when it exists. A stub
    daemon in a test has none; a failure costs the Work tab's state, never the hook's reply."""
    work = getattr(daemon, "_work", None)
    if work is None:
        return
    try:
        work.note(evt, req)
    except Exception as e:  # noqa: BLE001
        log.warning("work sessions: note %s failed (%s)", evt, type(e).__name__)


def _mcp_refusal(daemon: Any, req: dict[str, Any]) -> Optional[dict[str, Any]]:
    """None when the request carries the token of a live Work session and the chat is up; else the refusal."""
    work = getattr(daemon, "_work", None)
    if work is None or not work.check_token(req.get("sid"), req.get("token")):
        log.info("work sessions: refused an eve MCP request (no such session, or a wrong token)")
        return {"ok": False, "error": "not a live Work session eve started"}
    inlet = getattr(daemon, "_telegram", None)
    if inlet is None or getattr(inlet, "_chat_id", None) is None:
        return {"ok": False, "error": "eve's chat isn't running, or no owner has texted it yet"}
    return None

# Daemon._handle_ipc's dispatch table: event name -> handler (called as handler(daemon, req)).
IPC_HANDLERS: dict[str, Any] = {
    "expressions": Daemon._ipc_expressions,
    "explore": Daemon._ipc_explore,
    "lesson": Daemon._ipc_lesson,
    "notes": Daemon._ipc_notes,
    "trace": Daemon._ipc_trace,
    "pose": Daemon._ipc_pose,
    "axis": Daemon._ipc_axis,
    "follow": Daemon._ipc_follow,
    "move": Daemon._ipc_move,
    "celebrate": Daemon._ipc_celebrate,
    "species": Daemon._ipc_species,
    "diag": Daemon._ipc_diag,
    "identity": Daemon._ipc_identity,
    "session_start": Daemon._ipc_session_start,
    "session_end": Daemon._ipc_session_end,
    "turn_begin": Daemon._ipc_turn_begin,
    "turn_end": Daemon._ipc_turn_end,
    "pretooluse": Daemon._ipc_pretooluse,
    "permissionrequest": Daemon._ipc_permissionrequest,
    "push_character": Daemon._ipc_push_character,
    "unpair": Daemon._ipc_unpair,
    "sound": Daemon._ipc_sound,
    "mic": Daemon._ipc_mic,
    "chat": Daemon._ipc_chat,
    "mcp_tools": Daemon._ipc_mcp_tools,
    "mcp_call": Daemon._ipc_mcp_call,
    "get_state": Daemon._ipc_get_state,
    "posttooluse": Daemon._ipc_posttooluse,
    "notification": Daemon._ipc_notification,
}




def code_origin() -> str:
    """Which checkout and commit this daemon loaded: "<repo root> @ <short sha> (<branch>)". Logged at start, so a
    daemon running another worktree's code is visible on its first lines (2026-10-08: worktrees sharing one .venv
    re-pointed the editable install, and the live daemon ran a half-built branch with every /api/link path a 404)."""
    import subprocess
    from pathlib import Path

    here = Path(__file__).resolve().parent
    def git(*args: str) -> str:
        return subprocess.run(["git", "-C", str(here), *args], capture_output=True, text=True,
                              timeout=3).stdout.strip() or "?"

    try:
        root, sha, branch = (git("rev-parse", "--show-toplevel"), git("rev-parse", "--short", "HEAD"),
                             git("rev-parse", "--abbrev-ref", "HEAD"))
    except Exception:  # noqa: BLE001 — not a git checkout (an installed wheel): say where the code is
        return str(here)
    return f"{root} @ {sha} ({branch})"

def _log_permission_config_summary(matchers: MatcherConfig) -> None:
    """One-shot log at startup: how does the matcher interact with Claude Code's
    own permissions config? Flags the two most confusing misalignments:

    1. defaultMode == 'bypassPermissions' AND matcher is non-strict — only
       always_ask patterns are asked (on the phone, with the relay on); everything
       else is silently bypassed.
    2. matcher.strict but defaultMode unsuitable — strict mode wants
       bypassPermissions, otherwise unmatched commands still go through Claude
       Code's normal prompt UI.
    """
    import json

    from .claude_home import claude_config_dirs
    # The daemon serves every config home it was pointed at, so summarize the
    # first one that actually exists rather than assuming ~/.claude.
    candidates = [d / "settings.json" for d in claude_config_dirs()]
    settings_path = next((p for p in candidates if p.exists()), candidates[0])
    default_mode: Optional[str] = None
    ask_count = 0
    if settings_path.exists():
        try:
            with settings_path.open() as f:
                data = json.load(f)
            perms = data.get("permissions") or {}
            default_mode = perms.get("defaultMode")
            ask_count = len(perms.get("ask") or [])
        except (OSError, ValueError) as e:
            log.debug("could not read settings.json for permission summary: %s", e)

    matcher_summary = (
        f"matcher: strict={matchers.strict} "
        f"auto_allow={len(matchers.auto_allow)} "
        f"always_ask={len(matchers.always_ask)}"
    )
    log.info("%s", matcher_summary)
    log.info("code: %s", code_origin())
    log.info(
        "settings.json (%s): permissions.defaultMode=%r ask=%d",
        settings_path, default_mode or "(unset)", ask_count,
    )

    if default_mode == "bypassPermissions" and not matchers.strict:
        log.warning(
            "permissions.defaultMode='bypassPermissions' + matcher.strict=false: "
            "only always_ask patterns (%d defined) are asked, on the phone with the "
            "Telegram relay on; everything else is auto-approved without any "
            "human-in-the-loop. To ask about every un-vetted command, set "
            "`strict = true` in your matchers.toml.",
            len(matchers.always_ask),
        )
    elif matchers.strict and default_mode not in ("bypassPermissions", None):
        log.warning(
            "matcher.strict=true but permissions.defaultMode=%r: unmatched "
            "commands are asked on the phone (relay on) AND Claude Code may still surface "
            "its own terminal prompt depending on the mode. Strict mode is "
            "designed to pair with defaultMode='bypassPermissions'.",
            default_mode,
        )
