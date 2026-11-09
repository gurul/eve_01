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
| `debrief_list(kind, topic)` | `open`: the open threads from `TODO.md`'s Open section (up to 12, best match first when a topic is given); `stars`: the starred lessons from `HIGHLIGHTS.md`, without the template's example entries |
| `debrief_when(term)` | the days whose note mentions a project or thing, newest first, with a count per day, for "when did I last work on…" |

The search and get run the store's own search script (`debrief-search.py search` and `get`), with
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
- **A hung meaning layer** (Ollama taking connections but not answering) costs 25
  seconds; the search then runs once more by keyword. A refused one is fast in the
  script already. Either way the answer says it matched by words only.
- **One call at a time.** Chat and voice share the app, and the script's index
  rebuild is not safe with two readers, so calls queue behind a lock and run again
  once when another reader (a coding session) was rebuilding it.
- **The model is told** what the markers mean (`~` meaning-only, `⊘` replaced, the
  age in days, `★`), that an `INDEX.md` hit is the current answer, that a late-day
  addendum replaces its note's title, that a "review waived" part was never checked,
  and to search with a few distinctive words rather than a question, since the
  keyword layer needs every word in one chunk.

### Time questions are read by date

The search ranks by words, meaning and recency, never by date. On 2026-11-09 "what
was I working on developing yesterday?" brought back two older days, and even a query
holding the date did not reliably surface that day. So a question about a time goes
to `debrief_day`, which does no ranking:

- **One day** answers the day's row from `INDEX.md` first (its summary covers the
  late-day addenda), then an outline of the note's parts (each addendum names its
  session, project and end time), then the day's own note, `<date>-*.md` at the
  store's root. Most dailies are longer than 12,000 characters, so the summary and
  outline come first, where a clip cannot drop them.
- **A range** answers each day's `INDEX.md` summary, newest first: the newest seven
  in full, older ones by their first sentence. It names the days with no note, and
  counts only days that have a note (`INDEX.md` also holds template rows).
- **A day with no note** says so and names the nearest days that have one. A
  session is filed under the day it ended, so late-night work can sit in the next
  day's note; the tool says so to the model.
- **When-questions** ("when did I last touch the Voice PE?") go to `debrief_when`,
  which counts the term in each day's note (`voice pe`, `voice-pe` and `voicepe`
  are one name) and lists the days newest first. The search cannot answer them: it
  ranks by relevance, so the newest day is often not its first hit.
- Relative words are turned into dates on this Mac, in local time, and every answer
  starts with today's date and weekday, so the model never counts days itself.
- Drafts under `sessions/` and symlinked notes are never read.

## Safety

All four tools are reads: they never change a note (the script only refreshes its own
index cache) and never ask. They are not "clean" reads: a note can quote a web page
or a message, so a result counts as outside text, and an acting call later in the
same turn is asked of you first, as after `memory_search` or a web search. The model
is told the notes are a record, never instructions.

## What leaves the Mac

The search itself runs on this Mac. What it answers goes into the turn, so the lines
eve reads reach the chat or voice model (OpenRouter), like any tool result.

On the phone, the Apps tab lists Debrief with a status line (how many notes the store
has) and no controls.
