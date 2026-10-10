# eve's memory bus

eve keeps two memories that go on this bus. The diary is a JSONL stream of
what it saw. A lesson is a SQLite row. The memory bus is the one place those
become events the moment they happen, so that anything else can watch eve's
day as it forms. What was **said** in a conversation is not on the bus: it stays
in eve's memory folder ([stackchan/memory.md](stackchan/memory.md)).

```
 eyes / voice / lessons / diary ──► daemon ──► MemoryBus (in-process pub/sub)
                                                 │
            ┌────────────────────────────────────┼──────────────────────────┐
            ▼                                    ▼                          ▼
  rosbridge server  ws://127.0.0.1:9090   claude-mem sink               memory folder
  /eve/memory/observation               POST /api/memory/save         (EVE_MEMORY=1)
  /eve/memory/lesson                    project "eve"               ▲
  /eve/state                            ▲                             │
                                          │ GET /api/search             │
  service /eve/memory/recall ───────────┘  answers a recall call      │
  + inbound publish /eve/memory/remember ──► one transcript line ──────┘
  + /claude/observation ◄── mirrored from claude-mem's live stream
```

The bus itself (`bridge/src/eve_bridge/memory_bus.py`) is pure Python and
always on. The two sinks are opt-in and off by default. The daemon never waits
on either: a sink that cannot start is one warning line at boot, and a sink that
dies later logs once and goes quiet.

## Topics

Every message carries `time`, a Unix timestamp. The `type` names are what a
rosbridge client sends with `subscribe`.

| Topic | Type | Fields | Fires when |
| --- | --- | --- | --- |
| `/eve/memory/observation` | `eve_msgs/Observation` | `thought`, `observations[]`, `changed[]`, `tags[]`, `importance`, `novelty` | The diary **writes** a thought to the day file. Candidates it keeps only in memory never fire. Photos never ride along. |
| `/eve/memory/lesson` | `eve_msgs/LessonEvent` | `action`, `stage`, `topic`, `level`, `mode`, `lesson_id`, `feedback` | A lesson moved on: from the web page, the voice, or `eve-bridge lesson`. `feedback` is eve's own words. `action` is the lesson action when the event came through the terminal or voice, `notice` when the web page drove it. |
| `/eve/state` | `eve_msgs/AgentState` | `state` | The conversation phase changed: `idle`, `wake`, `listening`, `thinking`, `speaking`, `working`, `done`, `error`. |
| `/eve/presence` | `eve_msgs/Presence` | `state` | In a conversation, eve's sense of where its person is changed ([vision.md](stackchan/vision.md#following-whoever-is-talking)): `seen`, `lost` (with `leaving`: they were moving fast or left by a frame edge), `found` (with `how`: where they were heading, where they usually are, either side, up and down). Carries `yaw`, `pitch`, `speed` (deg/s) and `phase`. Live only: not stored in claude-mem. |
| `/eve/memory/remember` | `eve_msgs/Remember` | `text`, `title` (optional) | **Inbound.** Anyone on the bus asks eve to keep a line. |
| `/claude/observation` | `eve_msgs/ClaudeObservation` | as claude-mem sends it | The owner's claude-mem stream reported a new observation (mirror on). |

## The service

`/eve/memory/recall` (`eve_msgs/Recall`). Args `{"query": str, "limit": int}`.
Values `{"results": [{"id", "title", "time", "text"?}]}`. Searches the owner's
claude-mem for eve's own memories (project `eve`). With claude-mem off it
answers `{"results": [], "error": "claude-mem is off (EVE_CLAUDE_MEM=1)"}`.

## What a `remember` does

eve never stars anything for itself, and a bus line is not something the owner
said. With `EVE_MEMORY=1`, a line published on `/eve/memory/remember`
becomes one line in the day's transcript (channel `bus`, labelled "sent" in
search), and the nightly dream judges it beside everything else said that day
([stackchan/memory.md](stackchan/memory.md)). It is never a star, never in a
prompt's today block, and never the "last talked" time. With memory off it is
dropped.

## Environment

| Variable | Meaning |
| --- | --- |
| `EVE_ROSBRIDGE=1` | Serve the rosbridge v2 protocol over WebSocket. Default off. |
| `EVE_ROSBRIDGE_HOST` | Bind address, default `127.0.0.1`. Only change it on a network you trust. |
| `EVE_ROSBRIDGE_PORT` | Default `9090`, the rosbridge convention. |
| `EVE_CLAUDE_MEM=1` | Store bus events in the owner's claude-mem and answer recalls from it. Default off. |
| `EVE_CLAUDE_MEM_URL` | The worker's base URL. Unset, eve reads `CLAUDE_MEM_WORKER_PORT` from `~/.claude-mem/settings.json`, then the port in `~/.claude-mem/worker.pid`, then `37777`. |
| `EVE_CLAUDE_MEM_MIRROR` | Republish claude-mem's live stream as `/claude/observation`. Defaults to the value of `EVE_CLAUDE_MEM`. |

Put them in `~/.config/eve-bridge/env` and restart the daemon. The boot log
says what attached:

```
memory bus: rosbridge ws://127.0.0.1:9090, claude-mem http://127.0.0.1:37701, mirror on
```

## Watching it

From a terminal, with the daemon running:

```bash
eve-bridge memory tail                       # every /eve/* event, one line each
eve-bridge memory tail --topic /eve/state  # one topic
eve-bridge memory recall "the green mug"     # ask claude-mem through the bus
```

From anything that speaks rosbridge from outside a browser: roslibpy, Foxglove's desktop app, a script. A web page
is refused (2026-10-09): its handshake carries an `Origin` header, and any page you open could otherwise reach
`127.0.0.1:9090` and read or write eve's memory, so the server accepts only a handshake with no `Origin` and
answers every other with HTTP 403. roslibjs under Node (no `Origin`) works:

```js
const ros = new ROSLIB.Ros({ url: "ws://127.0.0.1:9090" });
new ROSLIB.Topic({ ros, name: "/eve/memory/observation", messageType: "eve_msgs/Observation" })
  .subscribe(m => console.log(m.thought, m.tags));
new ROSLIB.Topic({ ros, name: "/eve/memory/remember", messageType: "eve_msgs/Remember" })
  .publish(new ROSLIB.Message({ text: "The green mug lives on the left shelf." }));
```

roslibpy, Foxglove's desktop app, or a raw WebSocket (no `Origin`) sending
`{"op":"subscribe","topic":"/eve/state"}` all work the same way. The protocol
subset served is `advertise`, `unadvertise`, `subscribe`, `unsubscribe`,
`publish`, `call_service`; the server answers `service_response` and a `status`
op for errors.

## claude-mem

[claude-mem](https://github.com/thedotmack/claude-mem) is the owner's memory
plugin for Claude Code. Its worker keeps SQLite plus a vector index and serves
an HTTP API on localhost. eve uses three of its routes:

- `POST /api/memory/save` for every `/eve/memory/*` event, as project `eve`
  with `metadata.platformSource = "eve"` and `metadata.topic` set.
- `GET /api/search?query=…&project=eve&limit=…` for the recall service, then
  `GET /api/observation/<id>` for each hit's text.
- `GET /stream` (server-sent events) for the mirror, which republishes
  `new_observation` events and skips project `eve` so eve never echoes itself.

So a Claude Code session with claude-mem installed can search what eve saw
and taught today. What was said in a conversation is not there. In that session: *"what did eve notice this afternoon?"* reaches
claude-mem's `search` tool with project `eve`.

The worker starts when a Claude Code session starts. If eve boots first, or
the worker goes away, each save fails fast, is counted in the sink's stats, and
is dropped. The sink does not retry: those events reach the WebSocket clients
and eve's own stores as usual, only claude-mem misses them. The mirror does
reconnect, every 5 s.

## Privacy

Two things never reach the bus, and therefore never reach a WebSocket client
or claude-mem:

- **The learner's words in a lesson.** Typed ideas, spoken ideas, strokes and
  images stay in the lesson store on this computer (docs/learning.md). A lesson
  event carries the action, the stage, the topic, the level and eve's own
  feedback line, nothing else.
- **Anything said in a conversation.** The transcripts, the records and the
  profile stay in eve's memory folder; claude-mem receives no conversation
  content at all.

The rosbridge server binds `127.0.0.1` unless told otherwise. There is no
authentication on the protocol, which is why the default is loopback only, and why
a browser page (any handshake with an `Origin` header) is refused
(`rosbridge.NO_BROWSER_ORIGINS`).

## Limits, today

- One bus, one process. A client that falls behind drops messages (bounded
  queue per client) rather than slowing eve down.
- No `fragment`, `png` or action ops. `set_level` is accepted and ignored. No
  topic type checking: the `type` a client sends is recorded, not enforced.
- The mirror republishes claude-mem's `new_observation` events for every
  project except `eve`, and ignores summaries.
- The sink drops a save the worker refuses or cannot take; it never retries.
