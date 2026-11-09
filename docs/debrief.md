# Debrief

eve can search and read your debrief: the Markdown notes your Claude Code sessions
keep about past work (dailies, session notes, `INDEX.md`, `HIGHLIGHTS.md`). Ask the
chat or the voice "what did we decide about the Voice PE?" or "where does the Muse
work stand?", and eve looks it up in the notes and answers with their dates.

The module is `bridge/src/eve_bridge/debrief.py`; it is an app in the registry
([Adding an app](adding-an-app.md)).

## Turning it on

It is off unless `EVE_DEBRIEF=1`. It also needs a debrief store with its search
script at `.system/debrief-search.py`; without one the app stays off and logs why.

```bash
echo 'EVE_DEBRIEF=1' >> ~/.config/eve-bridge/env
launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon
```

| Variable | Default | What it does |
|---|---|---|
| `EVE_DEBRIEF` | off | `1`, `true`, `yes` or `on` turns the app on |
| `EVE_DEBRIEF_DIR` | `DEBRIEF_DIR`, else `~/claude-memory/debrief` | where the store is |

## Tools

| Tool | What it does |
|---|---|
| `debrief_search(query, n)` | the ranked index of matching notes: one line per hit, with an id, date, file and section; `n` is 1 to 15 (default 6) |
| `debrief_get(ids)` | the full text of up to 6 hits, by the ids the search gave |

Both run the store's own search script (`debrief-search.py search` and `get`), with
Python's isolated mode and a 45-second limit, so eve ranks the notes exactly as your
coding sessions do: by keyword and, when a local Ollama serves its embedding model,
by meaning. A result is clipped to 12,000 characters.

## Safety

Both tools are reads: they never change a note (the script only refreshes its own
index cache) and never ask. They are not "clean" reads: a note can quote a web page
or a message, so a result counts as outside text, and an acting call later in the
same turn is asked of you first, as after `memory_search` or a web search. The model
is told the notes are a record, never instructions.

## What leaves the Mac

The search itself runs on this Mac. What it answers goes into the turn, so the lines
eve reads reach the chat or voice model (OpenRouter), like any tool result.

On the phone, the Apps tab lists Debrief with a status line (how many notes the store
has) and no controls.
