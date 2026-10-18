# What eve remembers of talking with you

eve remembers two kinds of thing, and keeps them apart. What it **saw** is the
diary ([personality.md](personality.md)): thoughts about the room, under
`~/.config/eve-bridge/notes/`. What was **said** — by voice, by Telegram, in a
meeting it was asked to record — is this page: one memory folder, three stores and
an archive, one switch, one nightly dream, and one forget that reaches all of it.

It ships **off**. `EVE_MEMORY=1` turns it on. Off, no transcript, profile or
memory tool exists, and no brain gets one. One small thing is kept either way:
the Telegram **chat log** (`~/.config/eve-bridge/chat/log.json`, 0600, the
newest 300 lines per chat, 7 days at most), so a quiet spell or a restart does not
empty the chat's history. `EVE_CHAT_LOG=0` turns it off; see
[telegram.md](telegram.md#the-chats-history-the-chat-log).

## Why it looks like this

Until 2026-09-23 a conversation lived in RAM until it closed, went to one model
call to be distilled into a note, and was dropped. One failed call, one restart or
one crash and the words were gone. A Telegram chat could not see the voice chat of
an hour ago, Telegram was remembered only after ten quiet minutes, the voice could
not look anything up, and there were session notes, day files, an index, a
highlights file, records and a mem0 index, each written by a different loop. The
owner asked for it simpler (owner, 2026-09-23: "too many stores", "reconcile and
minimize") and for eve to "dream every night so memories can be fixed and
updated". So:

- **Every word is written the moment it is said.** A restart, a crash or a failed
  model call costs nothing, and the other channel sees it at once.
- **One nightly dream** turns a finished day into the records and the index. It is
  the only automatic writer of what eve "knows".
- **One facade** (`memory.py`) gives both brains the same context and the same
  tools, so what eve knows does not depend on how the owner reached it.

## The folder

```
~/.config/eve-bridge/memory/            0700   (EVE_MEMORY_DIR moves it)
├── transcripts/                              0700   SOURCE: every word (transcripts.py)
│   ├── <YYYY-MM-DD>.jsonl                    0600   one JSON line per turn, both channels
│   ├── meetings/<YYYY-MM-DD>/<HHMM>-<slug>.md 0600  meeting notes eve was asked to take (notes.py)
│   └── .metadata_never_index                        keeps Spotlight out
├── records/                                  0700   TRUTH: its own sealed git repository (records.py)
│   ├── profile.md                            0600   the one-pager every conversation starts with
│   ├── starred.md                            0600   what the owner said to remember, one dated line each
│   ├── hidden.md                             0600   what the owner said not to bring up again, one dated line each
│   ├── <id>.md                               0600   one typed record per thing
│   ├── days/<YYYY-MM-DD>.md                  0600   the dream journal: what happened, learned, still open, plans, corrected
│   └── .dreamt                               0600   the days already dreamt, one per line
├── mem0/                                     0700   INDEX: search by meaning, rebuilt from transcripts (mem0_memory.py)
│   ├── qdrant/  history.db  home/            0700 / 0600
│   └── ingested_convs                        0600   the conversations the index has read
├── index/memory.sqlite3                      0600   SEARCH: rounds, BM25 and vectors, only with EVE_MEMORY_HYBRID=1 (memory_index.py)
├── archive/                                  0700   the old debrief store, moved here once, still searched
└── .migrated                                 0600   the one-time move's counts
```

Modes are set in code, not left to the umask: folders are `chmod 0700`, files are
opened `0600` and `fchmod`ed again. The mem0 folder is re-tightened every time it
opens, because mem0 creates files of its own.

**A day starts at 04:00, not midnight.** A conversation at 01:00 belongs to the
evening it continued, in the transcripts, in a star's date and in the dream. It
keeps its real date all the same: the dream reads each line as `YYYY-MM-DD HH:MM`,
so the journal of 2026-09-23 lists an event of 00:08 the next morning as
2026-09-24 00:08. (Until 2026-09-24 the lines carried only `HH:MM` under
"Everything said on <day>", and `days/2026-09-23.md` filed exactly that event
under the wrong date.)

**The words never reach a git object, a cloud folder or a backup.** The transcripts
refuse (one WARNING naming the rule, then nothing is written) a folder that is
inside a git work tree, under `~/Documents`, `~/Desktop`, iCloud Drive or
`~/Library/CloudStorage`, a symlink, or not owned by this user. So
`EVE_MEMORY_DIR` must point somewhere outside those. The records' repository
is the `records/` folder itself, never the memory root, so a transcript can never
be committed.

### The three stores

| Store | Holds | Written by | Read by |
|---|---|---|---|
| **transcripts** | every line said or typed, with time, channel, conversation and speaker; what tools answered; meeting notes | the voice session and the Telegram inlet, live; the meeting recorder; a `/eve/memory/remember` bus line (as a channel `bus` line) | both brains (today's block, `memory_search`, `memory_read`, `recent_conversation`); the dream |
| **records** | typed records with dated facts and `[[id]]` links, the profile, the stars, the journal | the dream; "remember that" (to `starred.md` only); forget | both brains (profile, `memory_search`, `memory_read`); the widget (stars, journal) |
| **mem0 index** | facts about the owner, extracted by meaning, each with its day and channel | the dream (`ingest_day`, `tidy`); forget | `memory_search` (as `recalled`) |
| archive | the retired debrief store: session notes, day files, INDEX.md, HIGHLIGHTS.md and its git history | the migration, once; forget | `memory_search` (as `said`, channel `note`) |

A transcript line on disk:

```json
{"ts": "2026-01-01T18:30:05+00:00", "ch": "telegram", "conv": "t-1767292205000-a1b2", "who": "owner", "kind": "say", "text": "…"}
```

`ch` is `voice`, `telegram` or `bus`; `who` is `owner`, `eve`, `claude` or
`system`; `kind` is `say`, `tool`, `command`, `relay`, `image` or `close`. Nothing
is pruned: the owner keeps every day. A `bus` line is kept for the dream and for
search (labelled "sent"), but it is never in a prompt's today block, never in
`recent_conversation`, and never the "last talked" time: it is not the owner
speaking.

## Who writes what

Nothing else writes:

1. **Speaking and typing** → `transcripts.append`, the moment a turn closes (voice)
   or a message is typed or sent (Telegram). Both sides, the results eve answered
   from (a search, a look, `think_hard`, a memory lookup, a finished task), and a
   `close` marker when the conversation ends: on goodbye, after ten quiet minutes
   on Telegram, and at daemon shutdown for any conversation still open. **Think out
   loud (a lesson) writes nothing**: a learner's words are never kept.
2. **"Remember that"** — said out loud, or the Telegram `remember` tool —
   → `records.star` → one dated line in `records/starred.md`. It counts on the next
   message: the profile puts every star the dream has not absorbed yet on top of
   itself (above its first section), verbatim. The notes widget shows the
   stars; it does not write them.
3. **The nightly dream** → the records, the profile, the day's journal and the mem0
   index. It is the only automatic writer of the records, which keeps their first
   rule: **the brains read, they never write.** A model that promotes its own
   mid-conversation conclusions into what it "knows" launders a guess into a fact.
4. **Forget** (the owner confirms) → removes matching words from every store.
5. **Don't mention this again** (the app's Memory card) → `records.hide` → one
   dated line in `records/hidden.md`. It deletes nothing; see
   [Don't mention this again](#dont-mention-this-again).

## What each brain has

| | Telegram text brain | Voice: the Live front | Voice: the backend |
|---|---|---|---|
| In its prompt | `profile()` (≤ 6,000 chars, with the record index), today on both channels (≤ 32,000), the opening brief in the turn note | the opening brief, `profile_for_voice()` (≤ 3,000, no record index), today in a voice style (≤ 4,000) | `profile()`, today on both channels (≤ 16,000) |
| When it is read | every turn (a hand edit to the profile counts at once) | once, at session open (Live instructions cannot change mid-session) | once, at session open |
| Tools | `memory_search`, `memory_read`, `forget_preview`, `forget_apply` | none: it hands questions to the backend | the four, plus `recent_conversation` |
| `think_hard` gets | the voice profile and as much of today as fits in 6,000 | — | the profile and today, bounded to 6,000 by `think.py` |

The today block leaves out the lines the chat's own history already shows, so no
line is in the prompt twice. When today overflows its budget, the oldest lines go
and a header says how many, and names `memory_search`. The voice builds both
today blocks within 50 ms at session open; a slower disk opens the conversation
without them rather than holding the owner's first word.

**The opening brief** (`recall.opening_brief`) is one clause or nothing: the gap
since the last word said on either channel ("You last talked yesterday."), and, when
that was before today and the dream has written that day's journal, its title. A
talk less than fifteen minutes ago gets no clause. With memory off, or on the first
ever conversation, it is empty and eve greets as it always did.

## The tools

All five are defined once in `memory.py` with strict schemas and run through
`Memory.handle_tool` in a worker thread. The voice backend waits 2.5 s at most
(then it is told "Memory lookup took too long.") and sees at most 1,500 characters
of a result.

| Tool | Arguments | Returns |
|---|---|---|
| `memory_search` | `query`, `days_back` (null = 30) | three dated groups: `hits` (record fact lines with their ids), `recalled` (mem0 memories, as `(day, channel) fact`), `said` (transcript lines, meeting notes and archived notes, each with the line before and after). Every word of the query must be in a `said` line; a "quoted phrase" matches as written. |
| `memory_read` | `ref` | a record, by id; or a day of transcript, `YYYY-MM-DD`, optionally with `HH:MM` or `HH:MM-HH:MM`. A long stretch stops with "read again from HH:MM". |
| `recent_conversation` | — | voice only: what was typed on the other channel since this voice conversation opened. |
| `forget_preview` | `query` | counts per store and a token; see below. Changes nothing. |
| `forget_apply` | `token` | forgets what the preview counted. |

### Search: three switches, off by default

The memory plan's items 1, 4 and 5 (`memory_index.py`). Each has its own switch and stays off until the memory
eval's holdout run clears the bar in `bridge/tests/fixtures/memory_eval/BAR.md`. With all three off,
`memory_search` and its description are exactly the table above.

**Holdout verdict, 2026-10-08: all three stay off** (`bridge/tests/fixtures/memory_eval/VERDICT.md`). Overall
accuracy on the 162 holdout questions was 81.5% with every switch off, 96.3% with hybrid, 95.1% with retry and
82.1% with time ranges. Hybrid and retry each failed the bar on two gates. Abstention fell from 100% to 95.2%
(one question of 21). Forget compliance was 88.9%, a gate the baseline itself fails, because one forget question
leaks with every switch off. Time ranges also had 16% abstention errors.

- **`EVE_MEMORY_HYBRID=1`: ranked search over the words said.** Each round of a conversation (a line from the
  owner, or from the memory bus, and eve's reply after it) is one row of a local SQLite file,
  `memory/index/memory.sqlite3`, with an FTS5 (BM25) row and a 512-dimension `text-embedding-3-small` vector. A
  search scores every round in its days as `0.3 × BM25 (divided by the search's best) + 0.7 × cosine`, keeps
  rounds that share a telling word with the query or have a cosine of at least 0.30, and returns the best 8 as
  `said` hits (`text` the opening line, `before` the line before, `after` the reply), still under 3,000 chars.
  New lines reach the index off the event loop: `Transcripts.append` notifies a worker thread, which indexes the
  day 1.5 s after the last line and embeds the new rounds only after 30 s with no new line
  (`memory_index.EMBED_IDLE_SECS`), so a round that is still growing is not sent once per line; until then it is
  found by its words. A day whose file changed is also indexed again before it is searched, and every refresh that
  indexed a day drops the vectors no round uses. Vectors are kept by the hash of a round's text, so nothing is
  embedded twice. Both the BM25 candidates and the vectors scored come from the searched days only (one matrix
  product; a synthetic year of 4,380 rounds searches in about 11 ms, before the query's embedding call). Only the
  daemon runs the worker and rebuilds a missing file in the background; the `memory forget|dream|reindex`
  commands and the eval open the index without it. Until the index is built, or when the query's embedding fails
  (2 s timeout), the search is the keyword AND.
- **`EVE_MEMORY_TIMEARGS=1`: `from` and `to`.** Each is a day (`YYYY-MM-DD`) or the owner's words for a time
  ("yesterday", "last week", "in May", "two weeks ago", "last Tuesday", "since August 6"); code turns the words
  into days on the owner's 04:00 day (`memory_index.resolve_when`). The result carries the `range` it used. A range
  that finds nothing is widened once by its own length (at least a week) on each side, and says so (`widened`).
- **`EVE_MEMORY_RETRY=1`: search again before "I don't have that".** When the words said find nothing, the
  Memory searches again over 365 days and, without the ranked index, with half the telling words
  (`searched_again`); the result then says to search once more with other words. The Telegram brain allows at
  most three memory reads (`memory_search`, `memory_read`) in one turn, counted per turn (two turns at once keep
  their own count); a fourth is answered with "this turn has used its three memory reads"
  (`telegram.MAX_MEMORY_CALLS`). Only the Telegram tool description states that limit; voice has none, and its
  description does not claim one.

`bridge/tools/memory_eval.py --features hybrid,timeargs,retry` turns them on for an eval run (every switch it does
not name is set off). The search switches never rebuild the dream: hybrid adds one cached vector seed per data set,
and each question's copy indexes its own transcripts against it.

## The nightly dream

`dream.py`, started by the daemon when memory is on and `OPENROUTER_API_KEY` is set.
It wakes two minutes after start, then every 30 minutes, and dreams whatever
nights are due.

**Which nights are due.** Every transcript day before the current one that is not
in `records/.dreamt`. The day that just ended waits until 04:30 local time
(`DREAM_HOUR = 4.5`), half an hour after the 04:00 rollover, so a conversation that
ran past 04:00 has closed. Older nights are due at once: that is the catch-up after
the Mac slept through 04:30. Catch-up runs oldest first, **at most three nights a
wake**, so a week away does not become one burst of model calls.

**One night**, in a worker thread, never on the event loop:

1. `records.reconcile_day` — **one** model call reads the current records, the
   profile, the owner's stars and the whole day's transcript (at most 100,000
   characters), and returns the records that change (whole), the profile as it
   should read now, and the day's journal. Where the day contradicts a record, the
   record gets a dated correction and the journal says so. The journal's **Plans**
   keep when a plan happens apart from when it was said, one line each:
   `Dentist (when 2026-09-30 10:00; said 2026-09-23)`, or `when: no date yet`.
   "Tomorrow" and "Friday" are read from the date of the line that said them.
   Owner hand edits survive,
   because the current files are always its input. One commit, `dream: <day>`.
   **A forget during the dream wins.** The model call runs with the records
   unlocked, so a forget can land mid-dream; the night pins a forget generation
   (`records/.forgets`, a number) before it reads the day, and reconcile and
   consolidate write nothing if it moved. The night is dreamt again next wake from
   what is left. The mem0 ingest guards each add the same way, and forget finds and
   deletes index entries in one hold of its lock. Before 2026-09-25 a dream could
   write forgotten words back into the records and the git history the forget had
   just squashed (`verification/Eve/Forget.lean`).
2. `records.consolidate` — **one** model call over every record merges the same
   thing filed under two ids and dates a fact a newer one replaced. Skipped, with a
   log line, when the records pass 120,000 characters.
3. `index.ingest_day` + `index.tidy` — each conversation of the day the index has
   not read goes into mem0: what the owner said as the user, what eve said as the
   assistant, dated and labelled with its channel. Tool calls, commands, relays,
   photos and Claude's lines stay out. Then exact duplicates and any ★ candidate are
   removed.
4. The day is appended to `records/.dreamt` and a `## Dream` section with the
   counts is added to its journal.

A day with nothing said is marked with no model call. When the reconcile call
fails, nothing is written and the night is not marked, so the next wake tries
again; after three failures in one daemon run the night waits for the next start,
so one bad night cannot hold up the ones after it. A failure in consolidate or the
index still marks the night. Consolidate catches up on the next night, because it
reads every record. The index does not: the next night feeds it only that night's
conversations, so a day the index missed stays out of it until
`eve-bridge memory reindex`.

Every dream is one git commit in `records/`, so a wrong edit is one `git revert`
away and `git log -p` shows exactly what the model changed.

### Validity and sources on a fact (two switches, off by default)

Two switches change what a fact line carries (memory plan items 2 and 3, 2026-10-08).
Each ships off until the memory eval (`bridge/tools/memory_eval.py --features
validity,sources`) clears its bar. The extra fields go at the end of the line, in
braces:

**Holdout verdict, 2026-10-08: both stay off.** Validity scored 73.5% and sources 74.7%, against 81.5% with every
switch off. Most new misses were abstaining when the answer was there (see `VERDICT.md`).


```
- Rent is $1,180 a month. {said: 2026-08-29; valid_from: 2026-09-01; src: 2026-08-29 18:05}
```

A line with no braces is a plain fact, read and written exactly as before, so every
record file written before the switches loads unchanged.

- **`EVE_MEMORY_VALIDITY=1`** — a fact carries `said`, `valid_from` and
  `valid_to`. When the day contradicts a fact, reconcile **closes** it (sets
  `valid_to` to the date of the line that changed it) and adds the new fact beside
  it, instead of a prose correction; consolidate closes the older of two
  contradicting facts the same way. A fact about a plan whose date has passed is
  rewritten in the past tense. `profile()` shows only open facts (a profile line
  with a `valid_to` that has been reached is left out), and `memory_search` shows a
  closed fact labelled `(until YYYY-MM-DD)`, after the open ones on a tie.
  **The guard:** a night closes at most **6** facts
  (`records.MAX_CLOSES_PER_NIGHT`, shared by that night's reconcile and
  consolidate); any `valid_to` newly set counts, whatever its date. A real busy day
  (a move: address, rent, commute, landlord) closes four or five; more than six is
  more likely the model misreading the day, and a wrong closure hides a true fact
  from the profile. Past the limit the night is still written, so a day is never
  stuck or dreamt out of order: the first six closures stand, the rest stay open,
  and each one held open goes into that day's journal as a plain dated correction
  ("No longer true from YYYY-MM-DD: …").
- **`EVE_MEMORY_SOURCES=1`** — every fact reconcile writes carries `src`, the
  date and time of the transcript line it came from (`YYYY-MM-DD HH:MM`, the line's
  own date, which `memory_read` accepts as a ref). A `src` that is not the start of
  a line the model was shown, or one the records already carry, is dropped. The
  mem0 ingest sends one `add` per owner line (with eve's line before it as
  context) instead of one per 40 messages, so each memory's metadata carries the
  `src` of its line, and `src_before`, the time of eve's line, when the unit held
  one. `memory_search` shows the source on record hits (`"src"`, and
  `(source …)` in the line) and on recalled memories (in place of the day). **Forget
  follows the pointers:** the transcript lines a forget matches give their times,
  and every record fact and mem0 memory whose `src` (or `src_before`) is one of
  them is forgotten too, even when it is worded differently. Forget follows a
  pointer whenever one is stored, with the switch on or off. Not covered: mem0's
  own update step keeps the first memory's metadata when it merges a later line
  into it, so a forget of only that later line reaches the merged memory by its
  words, not by a pointer.

## Forget

"Forget that" used to remove one record file and leave the same words in nine
other places. Now it is two calls and one confirmation.

1. **`forget_preview(query)`** counts every place the words live and returns:

   ```json
   {"ok": true, "total": 7,
    "layers": {"transcripts": 4, "records": 1, "index": 1, "archive": 1, "meetings": 0, "chat_log": 2},
    "token": "…", "expires_in": 600, "not_covered": ["claude-mem", "vault", "chief ledger"]}
   ```

   It **never returns the matched words**: the owner decides on numbers, not on
   eve reading their secret back to them. The token is bound to that query, lives
   **600 seconds**, and works **once**.
2. eve tells the owner the counts and asks. The tool descriptions forbid calling
   `forget_apply` in the same turn: the owner confirms **in a new message**.
3. **`forget_apply(token)`** then:
   - rewrites every transcript line that matches to `(forgotten)`, keeping its time,
     channel, conversation and speaker (atomic rewrite under the writers' lock, so
     a concurrent append is never lost). When the line was the owner's, eve's next
     line in that conversation goes too: an answer like "Okay, no driving then."
     gives the fact away. The preview counts it;
   - removes matching lines from the records, the profile, `starred.md` and the
     journal (never frontmatter);
   - deletes matching mem0 memories and their rows in mem0's history database. When mem0 cannot be opened
     (its ten-minute retry window, no key) or cannot list its memories, the index is reported failed, never
     as nothing to forget;
   - whenever the search index file exists (with the hybrid switch on, or left from when it was on), deletes every
     round whose full stored text (the opening line, every reply line, or the line before it) matches and its FTS
     row, indexes every indexed day whose file changed again from the redacted transcripts, drops every vector no
     round uses, then rewrites the FTS segments and VACUUMs, every time, with `secure_delete` and FTS5's own
     secure-delete on (counted under `index`). A step that still fails after a few retries (a busy file) makes the
     result `ok: false` with `failed: ["index"]`, never a silent success; a failure in any other layer is reported
     the same way;
   - removes matching lines from the archive's and the meeting notes' markdown. A file that cannot be
     rewritten is not counted, and its layer (`archive` or `meetings`) is reported in `failed`;
   - removes matching lines from the Telegram chat log (the `chat_log` layer), and
     the chat's history with them. The daemon and `eve-bridge memory forget`
     both reach it: a daemon that finds the file rewritten by the CLI reads it
     again before its next write, so the forget is not undone. With no chat log
     (`EVE_CHAT_LOG=0`, or a log that could not be opened) the layer is
     absent and "telegram chat log" is listed under not covered;
   - **squashes the git history** of `records/` and of `archive/` to one commit of
     what is left, expires the reflog and prunes, so the words are not one
     `git log -p` away.

Matching is the one rule search uses: case-insensitive, every word longer than one
character must be in the line, a "quoted phrase" as written.

The stores the dream writes (records, mem0, the search index, the archive and meeting notes) also lose a line
the dream wrote **in its own words**. For an unquoted query with at least three distinctive words (common words
dropped, each word compared by its first four letters), a line goes when it holds at least three of them and at
least half. Forgetting "Honestly I failed my driving test back in July." takes "Avoid driving for now due to
failing the driving test in July" and leaves "no driving". The memory eval's holdout found this gap on
2026-10-08: with every switch off, 2 of 18 forgotten facts came back through the dream's rewording.

**Not covered**, and named as such rather than pretended: **claude-mem** and the
owner's **Obsidian vault** ([second-brain.md](second-brain.md)). Neither is eve's
store. The **chief ledger** ([chief.md](chief.md)) is eve's, but forget does not
reach it yet: its card titles and goals stay until a card is removed. Words already sent to a model provider or to Telegram are not reachable
either (see [Privacy](#privacy)).

### Don't mention this again

ChatGPT's control of the same name "reduces future references to the information.
It does not delete the original source." eve's does the same (2026-10-09). In
the app's Memory card, a long press on any row (a profile fact, or a search's fact,
meaning or line said) offers **Don't mention this again**. The row's words go into
`records/hidden.md` as one dated line (`records.hide`; saying it twice keeps one).
From the next read:

- `profile()` and `profile_for_voice()` leave out every page line and every fresh
  star the words match. Headings and the front matter stay.
- `memory_search` leaves out matching record facts, recalled meanings (mem0) and
  lines said (transcripts). `memory_read` of a record leaves the fact out too.

Matching is forget's rule (`transcripts.match`): a "quoted phrase" must appear as
written, otherwise every word must appear, in any case. Nothing is deleted: the
records, the profile page on disk, the transcripts and mem0 keep every word, and
the dream still reads them. The card's **Hidden** list (at the bottom of the
profile) takes a line back with **Unhide** (`records.unhide`). A forget that
matches a hidden line removes it from `hidden.md` too, as it does everywhere else.

## The one-time move from the old store

The first time the daemon starts with memory on, `memory.migrate` moves the old
debrief store (`~/.config/eve-bridge/debrief`, or wherever
`EVE_DEBRIEF_DIR` pointed it) and the old mem0 home
(`~/.config/eve-bridge/mem0`) under the memory folder:

| From | To |
|---|---|
| `debrief/records/` | `records/` |
| the owner's ★ lines under `## From talking` in `debrief/HIGHLIGHTS.md` (never the installer's worked example, never a `(candidate)`) | `records/starred.md`, with their dates |
| `mem0/` | `mem0/` |
| `debrief/notes/<date>/` (meeting notes) | `transcripts/meetings/<date>/` |
| everything else: `sessions/`, day files, `INDEX.md`, `HIGHLIGHTS.md`, `.git` | `archive/`, its repository sealed |

- **Renames only, never deletes.** Nothing is copied and nothing is removed. A
  rename across volumes is refused rather than turned into copy-and-delete.
- **A target that already exists is skipped** and counted, never overwritten.
- **Idempotent.** `.migrated` records the counts; while it exists the move does
  nothing. When a move failed, no marker is written and the next start tries again.
- With memory off, nothing under `~/.config/eve-bridge` is touched.

**Back up the old store before the first start with memory on:**

```bash
cp -Rp ~/.config/eve-bridge/debrief ~/eve-debrief-backup
cp -Rp ~/.config/eve-bridge/mem0   ~/eve-mem0-backup     # if it exists
```

The move is designed to lose nothing, but it rearranges the owner's only copy.
Keep the backup outside iCloud and git, and delete it once the new folder looks
right.

## Settings

| Variable | Default | What it does |
|---|---|---|
| `EVE_MEMORY` | `0` | The one switch. `1`: keep the transcripts, run the dream, give both brains the profile, today and the memory tools. `EVE_RECORDS=1` still works as a legacy alias. |
| `EVE_MEMORY_DIR` | `~/.config/eve-bridge/memory` | Moves the whole memory folder. The daemon, the CLI and the widget all read it. |
| `EVE_CHAT_LOG` | `1` | The Telegram chat log, kept with memory on or off ([telegram.md](telegram.md#the-chats-history-the-chat-log)). `0` turns it off. `EVE_CHAT_LOG_FILE`, `_LINES` and `_DAYS` move and bound it. |
| `EVE_MEM0` | on with memory | `0` turns just the mem0 index off. It is also off when `mem0ai` is not installed (`pip install -e ".[mem0]"`). |
| `EVE_MEM0_EMBEDDER` | `openrouter` | `gemma`: mem0 embeds on this Mac with EmbeddingGemma, in a store of its own (`mem0/gemma/`). An experiment; see [Local embeddings](#local-embeddings-experiment). Any other value is `openrouter`. |
| `EVE_MEMORY_VALIDITY` | `0` | `1`: facts carry said / valid_from / valid_to; the dream closes a superseded fact instead of correcting it in prose, at most 6 a night. See [Validity and sources](#validity-and-sources-on-a-fact-two-switches-off-by-default). |
| `EVE_MEMORY_SOURCES` | `0` | `1`: every fact the dream writes and every mem0 memory carries the date and time of its transcript line; search shows it, forget follows it. |
| `EVE_MEMORY_HYBRID` | `0` | `1`: `memory_search` ranks the words said by BM25 and embeddings over a local index (`memory/index/`); see [Search: three switches](#search-three-switches-off-by-default). |
| `EVE_MEMORY_TIMEARGS` | `0` | `1`: `memory_search` takes `from` and `to` (a day, or words like "last week"), resolved in code on the 04:00 day. |
| `EVE_MEMORY_RETRY` | `0` | `1`: an empty search is tried again wider by code, and the Telegram brain allows at most three memory reads a turn. |
| `EVE_DEBRIEF_DIR` | `~/.config/eve-bridge/debrief` | Read only as the source of the one-time move. |
| `OPENROUTER_API_KEY` | — | Needed by the dream, by mem0 and by the hybrid search's embeddings, all through OpenRouter ([one key: OpenRouter](../openrouter.md)). Without it the records are read but never dreamt, and there is no index. The embeddings are the same `text-embedding-3-small` at the same dimensions as before the move (512 for the search index, 1536 for mem0), so the stored vectors did not need rebuilding. |

Everything else is a constant in code, not a knob: the 04:00 day start, the 04:30
dream, the budgets above, the models (`gpt-6-luna` at low effort for the dream and mem0's
extraction, `text-embedding-3-small` for mem0's embeddings).

## Local embeddings (experiment)

**On for the owner since 2026-10-09** (owner: "if it works switch should be on"): `EVE_MEM0_EMBEDDER=gemma`
in the env file, the gemma store built with `memory reindex` (17 days, 5.5 minutes, the daemon stopped). The
daemon warms the model on a thread at start (`OwnerMemory.warm`), so the first search after a restart does not
pay the ~3 s load against the 3 s cap. The score cut-off (`MIN_SCORE` 0.25) holds for it: on the experiment's
made-up corpus it keeps 97% of right answers and lets 3.9% of unrelated ones through. Unset the variable to go
back to the OpenRouter store, which is kept.

`EVE_MEM0_EMBEDDER=gemma` makes mem0's embeddings local: EmbeddingGemma
(`embeddinggemma-300m`, 768 dimensions) through mem0's huggingface embedder, with
the model card's task prompts (a search is a query, a stored memory is a
document). Fact extraction still goes to OpenRouter. The default, `openrouter`,
is unchanged.

The gemma store is separate: `mem0/gemma/` holds its own Qdrant collection
(`eve_gemma`), history database and `ingested_convs`. The 1536-dimension store
in `mem0/` is never opened with the switch set. Google's model repo is gated, so
the code reads an ungated copy (`unsloth/embeddinggemma-300m`, pinned to one
revision) from the local Hugging Face cache only. The daemon never downloads it.

To build the gemma index (daemon stopped, model fetched once, about 1.27 GB):

```bash
pip install -e ".[mem0,gemma]"
python -c "from sentence_transformers import SentenceTransformer as S; S('unsloth/embeddinggemma-300m', revision='bfa3c846ac738e62aa61806ef9112d34acb1dc5a')"
EVE_MEMORY=1 EVE_MEM0_EMBEDDER=gemma eve-bridge memory reindex
```

Blind eval, 2026-10-09 (`bridge/tools/evals/embeddinggemma/`: 40 made-up
snippets, 30 questions, bar written first): **pass**.

| | openrouter | gemma |
|---|---|---|
| recall@5 (of 30) | 30 | 30 |
| recall@1 | 28 | 28 |
| query embedding, median / p95 | 551 / 685 ms | 20 / 54 ms |
| peak RAM | 126 MB | 1035 MB |
| install | — | +0.8 GB venv, 1.27 GB model |

Both arms hit the ceiling of this set, so it shows gemma is not worse here, not
that it is better. The cost is RAM: about 0.9 GB more for as long as the daemon
holds the model. Loading it takes about 3 s, so the first search after the
daemon starts can pass the 3 s search cap and answer without mem0. The hybrid
search index (`memory_index.py`) still embeds through OpenRouter.

## Compared with ChatGPT's memory (read 2026-10-09)

Owner, 2026-10-09: "use this article to reverse engineer how memory works for
them" (help.openai.com, *Memory in ChatGPT*, updated 2026-09). What the article
states, and what eve has for each:

| ChatGPT | What the article says | eve |
|---|---|---|
| **Sources** | Memory draws on past chats, saved memories, custom instructions, Library files and connected apps (Gmail). "ChatGPT decides which available information is relevant to a response." | The transcripts (past chats), the records' facts and stars (saved memories), the profile and persona (instructions), the second brain's notes (files). Connected apps are read live, never kept. |
| **Two experiences** | *Improved memory* "continually updates a broader summary of relevant context"; *legacy saved memories* are "a more explicit list of individual memory items". | Both at once: the profile is the summary, dreamt nightly from the transcripts (not continually); the stars are the explicit list. |
| **Memory summary** | "A high-level view … does not necessarily include every detail." Editable: a correction in a text field, highlight-and-correct, *Don't mention this again*, *Delete and turn off memory*. | The profile is the summary, shown in the app's Memory card. Correct it by remembering the corrected claim (a star outranks a dreamt fact at the next dream). *Don't mention this again* on any row hides the words from the profile and every search, deleting nothing ([below](#dont-mention-this-again)); forget is the deletion, everywhere. |
| **Retrieval** | "Does ChatGPT search my history for every response? No. ChatGPT looks for relevant context when it is likely to improve a response." | The same shape: the profile and today's talk ride every turn; the stores are searched only when the brain calls `memory_search` (it decides). |
| **Chat history** | Derived information "can change as ChatGPT updates what is most useful"; no storage limit; turning it off schedules deletion within 30 days. | The dream re-reads each night and rewrites facts with their validity; transcripts are kept whole, pruned by the owner's forget only. |
| **Saved memories** | Stored apart from chats; deleting the chat does not delete the memory; controls may include search, sorting, automatic prioritization and version history. | `starred.md`, apart from the transcripts, in a git repo (version history); searched with the rest; no prioritization. |
| **Sources under a reply** | "Sources may appear below the response": which chat, memory, file or email contributed; mark relevant or not; delete a referenced chat. | Shown in the app (2026-10-09): a reply that read memory has "Sources · N" under it, the facts, recalled meanings and lines said that its `memory_search` / `memory_read` calls found (at most 6), kept on the reply's transcript line as `src` and never shown to the model ([app-chat.md](../app-chat.md), "Sources"). No mark-relevant or delete on a source yet; forget removes a quoted one. Logs stay counts only. |
| **Forget** | Deleting a saved memory "does not remove mentions of that information from past conversations"; the owner must also delete the chat, files and app data. Logs of deleted memories are kept 30 days. | One forget hits every store (facts, meanings, transcripts, notes, chat log) and squashes the git history behind them. |
| **Temporary chat** | No memories created or used; chosen before the chat starts; cannot be changed after. | Stealth (`set_stealth`) keeps a stretch out of the transcripts. |
| **Scope** | Project-only memory: chats in a project reference only that project. | One owner, one memory; no scoping. |
| **Searches** | Memory personalizes web searches ("location or dietary preferences can help refine a restaurant search"). | The search router gets eve's self-context, not the owner's profile. |

What is worth taking from it, in order: (1) *Don't mention this again*, a
suppression that is not a deletion (built 2026-10-09); (2) sources under a reply,
so the owner sees which memory lines an answer leaned on (shown since 2026-10-09;
marking one wrong is still to do); (3) the profile feeding the web search's query
for place and diet (measured 2026-10-09: web searches already carry it; OpenTable
deliberately does not infer a place from saved location and asks instead).

## From the app

Eve Link's Apps tab has a **Memory** card (2026-10-09; [app-chat.md](../app-chat.md),
"Work"): the profile at rest (as sections of facts), a search over the three stores,
**+** to remember something (`Memory.star`), *Don't mention this again* on a
row's long press with a **Hidden** list to undo it (`Memory.hide`), and Forget with the same
preview-then-token two-step as the chat's tools, through
`POST /api/link/controls/memory` (`link_memory.py`). It is listed only while the
daemon's memory is on and built, so it needs no extra setup.

## From the terminal

```bash
eve-bridge memory forget "<words>"   # preview the counts, then [y/N], then forget everywhere
eve-bridge memory dream               # dream every night that is due now
eve-bridge memory dream --day 2026-01-01   # dream that day again, even if already dreamt
eve-bridge memory reindex             # rebuild the mem0 index from every transcript day
```

All three need `EVE_MEMORY=1` in the shell, as the daemon has it, and refuse
until the daemon has started once with memory on (the one-time move is the
daemon's, and `.migrated` must exist). They print counts, never the matched words.
`dream --day` refuses the current transcript day and any later one: that day is
not over, and dreaming it would mark it dreamt, so tonight's dream would skip the
rest of it.

**Run `dream` and `reindex` with the daemon stopped.** mem0's on-disk store takes
one opener at a time. The CLI does not stop the daemon for you: `reindex` checks
afterwards and says "stop the daemon first" when the index did not open. A
`dream` run beside the daemon still writes the records, but the index cannot open,
so that day stays out of the index until a `reindex`. The same holds for `forget`
from the terminal: with the daemon running, the index cannot be opened and its
count is 0, so forget from a chat (the daemon's own index) or stop the daemon
first. `reindex` clears `mem0/ingested_convs` (`mem0/gemma/ingested_convs` with
`EVE_MEM0_EMBEDDER=gemma`) and feeds every transcript day again;
mem0's own update step and `tidy` absorb what was already there.

## Privacy

The memory folder is this Mac's. Here is exactly what leaves it, and when.

- **The Telegram text brain** (`gpt-6-luna`, OpenAI Responses API, `store=False`)
  is sent the profile, today's lines on both channels and each memory tool's result,
  every turn. Telegram's own servers carry the chat itself, as they always did.
- **The voice** (OpenAI's Live API and its Responses backend) is sent the voice
  profile and today's lines at session open, and the backend the full profile,
  today, and each memory tool's result.
- **`think_hard`** (`store=False`) is sent the profile and up to 6,000 characters of
  today with the question.
- **The dream** (`gpt-6-luna`, `store=False`) is sent one day's transcript (at most
  100,000 characters), the records, the profile and the stars, once a night; and all
  the records for consolidate.
- **mem0** sends the owner's side of each conversation, and eve's replies, to
  OpenAI for fact extraction (`store=False`), and each fact and each
  `memory_search` query for an embedding (the embeddings call has no such flag).
  Its vector store and history database stay on disk. mem0's PostHog
  telemetry is forced off before the first import, and mem0 is refused if it still
  reads on; `MEM0_DIR` keeps its config out of `~/.mem0`.
- **claude-mem receives no conversation content.** The memory bus carries no
  conversation topic ([memory-bus.md](../memory-bus.md)): what was said stays in the
  memory folder. A `/eve/memory/remember` line sent in over the bus becomes one
  transcript line for the dream to judge, never a star.
- **The memory's logs carry counts, channels and milliseconds**, never a word that
  was said and never a file name (a journal's name summarises its day). The Telegram
  inlet logs no words either. One exception, older than this memory and local to the
  daemon's log file: the voice session logs eve's own spoken replies, what the
  backend handed the voice, and the first words of an owner turn it classified as a
  command, so a wrong reply can be traced. A learner thinking out loud is never logged.
- **The git repositories cannot be pushed.** `records/` and `archive/` are sealed on
  every start (`records.seal`): any remote is removed, every push URL is rewritten
  to one that cannot resolve (which `--no-verify` cannot skip), a pre-push hook
  refuses, and `core.hooksPath` points at it so a global hooks path cannot bypass
  it. A records folder inside some other repository is never committed to at all.

Nothing in `transcripts.py` imports a network library. FileVault is what protects
the folder at rest.

## Where to look in the code

`bridge/src/eve_bridge/`: `memory.py` (the facade, the tools, forget, the
move), `memory_index.py` (the search index, the time resolver, the search switches), `transcripts.py`, `records.py`, `mem0_memory.py`, `dream.py`, `recall.py`
(the folder layout and the opening brief). The brains: `telegram.py`,
`voice_agent.py`. The CLI: `cli.py` (`memory forget|dream|reindex`).

### Model routing (October 9)

Fact extraction, nightly reconciliation and record compaction use Luna at low
effort. Compaction has its own client model field so changing extraction does
not implicitly change compaction. mem0 explicitly marks its extraction model
as reasoning-capable and keeps the 2,000-token response cap. Embedding models,
dimensions, stores and memory prompts are unchanged. See the
[model role table](../openrouter.md#models-by-role).
