# Debrief

eve can search and read your debrief: the Markdown notes your Claude Code sessions
keep about past work (dailies, session notes, `INDEX.md`, `HIGHLIGHTS.md`). Ask the
chat or the voice "what did we decide about the Voice PE?", "where does the Muse
work stand?" or "what was I working on yesterday?", and eve looks it up in the notes
and answers with their dates.

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

`EVE_DEBRIEF_DIR` is also the name memory's one-time migration reads for eve's own
retired store. The migration never moves a folder that holds
`.system/debrief-search.py`, so setting it for this app cannot empty your notes.

## Tools

| Tool | What it does |
|---|---|
| `debrief_search(query, n)` | the ranked index of matching notes: one line per hit, with an id, date, file and section; `n` is 1 to 15 (default 6) |
| `debrief_get(ids)` | the full text of up to 6 hits, by the ids the search gave; each id gets an equal share of the 12,000 characters |
| `debrief_day(date, until)` | a day or a range, read by date: `date` is `YYYY-MM-DD`, `today`, `yesterday`, `day before yesterday`, a weekday (its most recent past day), `latest` (the newest day with a note), or a span word (`this week`, `last week`, `weekend`, `this month`, `last month`); `until` is a range's last day, up to 62 days, or null |
| `debrief_status(name)` | where a project, feature or device stands: its glossary entry in `INDEX.md` (kept current), sentences in other entries that name it (a later state often lives there), the last days it was worked on, and its open threads |
| `debrief_list(kind, topic)` | `open`: the open threads from `TODO.md`'s Open section (up to 12, best match first when a topic is given); `stars`: the starred lessons from `HIGHLIGHTS.md`, without the template's example entries |
| `debrief_when(term)` | the days whose note mentions a project or thing, newest first, with a count per day, for "when did I last work on…" |

Every tool runs the store's own script (`debrief-search.py`: `search`, `get`, `day`,
`when`, `status`, `todos`, `stars`), with
Python's isolated mode and a 45-second limit, so eve ranks the notes exactly as your
coding sessions do: by keyword and, when a local Ollama serves its embedding model,
by meaning. A result is clipped to 12,000 characters.

What the search and get add on top of the script (2026-11-09, from a swarm that put
past-work questions to the real store):

- **Each id is read on its own** with an equal share. A hit longer than its share
  (`INDEX.md`'s systems section is one chunk of about 160,000 characters) is cut to
  the entries that name the last search's words, so the current answer, which
  `INDEX.md` usually holds, is what reaches the model.
- **Ids are checked.** The script rebuilds its index whenever a note changes, and the
  ids shift. eve remembers each id's file and section from the search and refuses a
  get whose header no longer matches ("search again"), rather than answer from
  another note.
- **A search does not hang.** The store's script (claude-debrief) waits at most 8
  seconds for the query's embedding and spends at most 3 seconds embedding new
  notes, handing the rest to a background run. eve keeps a 15-second backstop:
  past it, the search runs once more by keyword. Either way the answer says when
  it matched by words only.
- **One call at a time.** Chat and voice share the app, and the script's index
  rebuild is not safe with two readers, so calls queue behind a lock and run again
  once when another reader (a coding session) was rebuilding it.
- **The model is told** what the markers mean (`~` meaning-only: read before trusting, never a
  reason by itself to say there is no record, since the blind eval found the right note among
  all-`~` results 14 times in 20; `⊘` replaced, the
  age in days, `★`), that an `INDEX.md` hit is the current answer, that a late-day
  addendum replaces its note's title, that a "review waived" part was never checked,
  and to search with a few distinctive words rather than a question, since the
  keyword layer needs every word in one chunk.

### Questions with structure are not searched

The search ranks by words, meaning and recency, never by date. On 2026-11-09 "what
was I working on developing yesterday?" brought back two older days, and even a query
holding the date did not reliably surface that day. A swarm then found the same shape
in "when did I last…", "where does X stand" and "what's left to do".

The notes already have structure, so the store's script (claude-debrief) now builds
tables from the same Markdown whenever a note changes: `days` (each daily and its
`INDEX.md` summary), `sessions` (from each daily's Sessions table and addendum
headings: id, project, end time), `entities` (the `INDEX.md` glossary of `[[name]]`
entries), `mentions` (which `[[name]]` each section links), `todos` (`TODO.md`, open
and closed) and `replaced` (`SUPERSEDED.md`). The Markdown stays the source of truth.
eve's table tools are thin calls to it, so a coding session asking the same script
gets the same answer.

- **One day** (`day`) answers the day's summary, its sessions with project and end
  time, an outline of its parts, then the note. Most dailies are longer than 12,000
  characters, so everything before the note survives the clip.
- **A span** answers each day's summary, newest first (the newest seven in full,
  older ones by a first sentence), and names the days with no note. Template rows in
  `INDEX.md` never count: a day needs a daily.
- **A day with no note** says so and names the nearest days that have one. A
  session is filed under the day it ended, so late-night work can sit in the next
  day's note; the tool description says so.
- **Time words** (`yesterday`, a weekday, `last week`, `weekend`, `last month`) are
  passed through as the owner said them; the script turns them into dates in local
  time, and every answer starts with today's date and weekday.
- **When** counts a name in each daily, links and prose alike (`voice pe`, `voice-pe`
  and `voicepe` are one name), newest first.
- **Status** finds the glossary entry a spoken name means (`voice pe` →
  `voice-pe`), and also quotes the sentences in other entries that name it: when the
  Kokoro voice became opt-in again, that was recorded in the call's entry, not
  Kokoro's.
- Drafts under `sessions/` are never read.

An older script without these commands answers "update claude-debrief" rather than
failing silently.

### Measured (2026-11-09)

- **Routing**, blind and pre-registered ([evals/debrief-routing-2026-11-09](evals/debrief-routing-2026-11-09/VERDICT.md)):
  the chat brain reached the debrief on 26 of 26 past-work questions (24 before
  these tools) and on none of 14 that belong elsewhere (chat history, memory, the
  to-do list, the calendar, GitHub, the notes vault).
- **"No record"**: on a fresh blind set of 30, the production chat model with these
  tools answered 15 of 15 answerable questions and declined all 15 about things
  never recorded, searching again on its own when the first hits did not answer. A
  stricter reading instruction scored the same and was not adopted; the meaning
  score cannot make this call (claude-debrief `docs/search.md`).
- **Embedding model**: EmbeddingGemma 2 kept after a blind comparison with
  Qwen3-Embedding (0.6B, 4B) and BGE-M3 (claude-debrief `docs/search.md`).

## Safety

All the tools are reads: they never change a note (the script only refreshes its own
index cache) and never ask. They are not "clean" reads: a note can quote a web page
or a message, so a result counts as outside text, and an acting call later in the
same turn is asked of you first, as after `memory_search` or a web search. The model
is told the notes are a record, never instructions.

## What leaves the Mac

The search itself runs on this Mac. What it answers goes into the turn, so the lines
eve reads reach the chat or voice model (OpenRouter), like any tool result.

On the phone, the Apps tab lists Debrief with a status line (how many notes the store
has) and no controls.
