# The turn ledger: where a turn's seconds go

eve's speed work is ranked by wall time saved. Before the ledger, the only
per-turn timing was one log line ("turn answered in X s"). Model calls had no
latency, only memory tools were timed, and nothing joined a turn's spend rows
to its tool calls. The ledger records every turn's timings under one trace id,
so "what costs the most seconds" is a query, and every later speed change has a
before and an after.

Code: `bridge/src/eve_bridge/turn_ledger.py`. Tests:
`bridge/tests/test_turn_ledger.py`.

## What is written

One JSON line per event, in a per-day file named by the local date:
`~/.config/eve-bridge/turns/<YYYY-MM-DD>.jsonl`.

```json
{"t": 1790000000.123, "ev": "turn.started", "trace": "4bf92f3577b34da6", "door": "app"}
{"t": 1790000000.412, "ev": "model", "trace": "4bf92f3577b34da6", "door": "app", "model": "gpt-6-luna", "ms": 289, "in": 3100, "cached": 2900, "out": 80, "usd": 0.00042}
{"t": 1790000000.700, "ev": "call", "trace": "4bf92f3577b34da6", "door": "app", "tool": "web_search", "ok": true, "ms": 280, "gated": false}
{"t": 1790000001.020, "ev": "turn.settled", "trace": "4bf92f3577b34da6", "door": "app", "outcome": "answered", "rounds": 2, "total_ms": 897}
```

| Event | Written by | Fields |
|---|---|---|
| `turn.started` | each text turn of the chat brain (`telegram.py`, `TelegramInlet._turn`), once the turn lock is held | `door`; `parent` when one turn ran inside another |
| `turn.settled` | the same turn, on the way out | `outcome` (`answered`, `silent`, `small_talk`, `failed`, `error`, `cancelled`), `rounds`, `total_ms` |
| `model` | each chat model call in the round loop, the out-of-rounds summary, and the small-talk lane's call | `model`, `ms`, `in`, `cached`, `out` (this call's tokens), `usd` (null when not priced) |
| `call` | every tool call through `TelegramInlet._tool` | `tool`, `ok`, `ms`, `gated` (the taint gate asked first) |
| `gate` | each taint-gate question (`_gated_tool`) | `tool`, `allowed`, `ms` (how long the asking took, the owner's time included) |
| `link` | a Mac task's run log (`computer_agent.py`), Codex's own turn id (`codex_computer.py`) | `kind` (`agent_run` or `codex`), `ref` |

- **The door** is `call` for a turn begun on a phone call, `app` for an Eve
  Link app turn, and `telegram` for the rest (Telegram, the desk controller and
  every other door that hands the inlet text). A tool call made outside a turn
  (a Work session's call into eve's tools) is written with door `other` and no
  trace.
- **The trace id** is 16 hex characters, kept in a context variable. Every row
  written inside the turn carries it, including rows from tasks and threads the
  turn starts. Two turns at once get two ids.
- **Spend rows join on it.** A spend line written during a turn carries
  `"trace": "<id>"` ([what eve spends](stackchan/spending.md)). A Mac task's run
  log under `agent-runs/` carries the trace in its first line, and the turn's
  `link` row names the run log, so each can be found from the other.
- **Names and numbers only** (AGENTS.md rule 9). Each event has an allowlist of
  keys, and every other key is dropped before the line is built. Every string
  must look like a name (letters, digits and `_.:/-`, no spaces, at most 80
  characters) or it is written as `?`, so no sentence, email address or phone
  number can reach the file, whatever a caller passes. A single bare word could,
  so the callers pass only names and numbers: never message text, a tool's
  arguments or result, a recipient or a token.
  `bridge/tests/test_turn_ledger.py` checks this with a sentinel string in the
  message, the tool's arguments, its result and the reply. A deliberately leaky
  writer fails the same check, which proves the check can fail.
- **It never breaks a turn.** A write that fails (a full disk, a folder that
  cannot be made) is one log line with the error's type, and the turn goes on.
  The turn's own exception always passes through.
- **Cost.** About 0.3 ms for a turn of nine events (median of 20 in a
  microbenchmark on the owner's Mac; 0.003 ms with the ledger off).
  The files are not pruned. At about 3 KB a turn they grow by well under a
  megabyte a day.

Not in the ledger yet: the desk voice's own turns (`voice_agent.py`), the
computer agent's steps (its run log keeps those), and `audit.py`, the Claude
Code permission log, which holds command text and stays separate.

## Reading it

Both commands read the files offline; the daemon need not run.

```bash
bridge/.venv/bin/eve-bridge turns                     # today
bridge/.venv/bin/eve-bridge turns --day yesterday
bridge/.venv/bin/eve-bridge turns --day 2026-10-26 --tool web_search
bridge/.venv/bin/eve-bridge trace 4bf92f3577b34da6    # a unique prefix of 6 or more will do
```

`eve-bridge turns` prints three tables, each sorted slowest p95 first:

- **Doors**: whole turns (`total_ms`) per door, with how many failed.
- **Tools**: tool calls per tool, with how many returned `ok: false`.
- **Models**: model calls per model, with their dollars.

Each row shows the count and the p50 and p95 in milliseconds (nearest rank).
`--tool T` keeps only that tool's calls, and only the doors and models of the
turns that made one.

`eve-bridge trace <id>` prints one turn's rows in order, each at its offset from
the turn's start, then the spend rows with the same trace from the same days.
It looks back 14 days (`--days N`).

## Settings

| Setting | Default | What it does |
|---|---|---|
| `EVE_TURN_LEDGER` | on | `0` turns the ledger off: no file, no trace id, and spend lines exactly as before. |
| `EVE_TURNS_DIR` | `~/.config/eve-bridge/turns` | Where the day files go. Both commands also take `--dir`. |

eve lite keeps the ledger on, with the same rule on words
(`profile.LITE_CLASSIFIED`).

## Checking it on the Mac

Send three phone turns: one small talk, one that uses a memory tool and a web
tool, and one that hits a gated tool. Then:

- `eve-bridge turns --day today` lists each tool with a count, p50 and p95.
- `eve-bridge trace <id>` for the second turn shows the turn, at least two
  calls, at least one model row with `usd`, and its spend rows.
- `total_ms` is within 5% of that turn's "turn answered in" log line.
- `grep -F` for each message's text finds nothing in `turns/*.jsonl`. The same
  grep against the chat log is the positive control and finds it.
