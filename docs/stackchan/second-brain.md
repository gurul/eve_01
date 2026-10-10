# The second brain: your notes, texted to eve

eve's memory ([memory.md](memory.md)) is what eve knows about you, rewritten
only by its nightly dream and never mid-conversation.
The second brain is the other thing: what **you** write down and want back
later. A thought texted from the bus, a todo, a line for the journal, a meeting
to distil. `second_brain.py` turns a text to eve into a markdown file in your
Obsidian vault in seconds, lets eve look things up in it, and compiles the
vault into prompts for four workflows: plan my day, weekly review, triage my
inbox, distill this transcript.

You said it on 2026-09-21: *"the second brain system is for my personal notes
and things of sort I'll text to telegram"*, and *"obsidian is on the mac rn"*.
The design is the one in the deck you sent the same day, "The AI Second Brain:
Building Your Personal OS for the Age of Agents" (Patrick Ellis, Feb 2026): a
local markdown vault organised PARA-plus, zero-friction capture, agent
workflows driven by the vault, and context packs that compile it into one
prompt.

It ships **off** (`SECOND_BRAIN_DEFAULT = False`). When it is on, the text
brain gains nine tools and the vault skeleton is written on first use.

```
your phone ── Telegram ──▶ telegram.py text brain
                              │ capture_note            "note: the pasta place is Doppio"
                              ▼
                 ~/Documents/Second Brain/           (the Obsidian vault; EVE_VAULT moves it)
                   01-inbox/2026-09-21-1830-the-pasta-place-is-doppio.md
                   02-todos/master.md        ◀── "todo: p1 book the dentist"
                   08-journals/daily/2026-09-21.md  ◀── "journal: felt sharp this morning"
                              │ search_notes · read_note · edit_note · undo_note · list_inbox · list_todos · file_note
                              │ second_brain_workflow → one prompt → think_hard → your phone
                              │   (daily-plan: the text brain reads your calendar and writes the plan itself)
```

## Where the vault is

Obsidian was installed on this Mac the day this was built, and had no vault
yet: its registry (`~/Library/Application Support/obsidian/obsidian.json`) did
not exist, and no `.obsidian/` folder existed anywhere on disk. So the second
brain **is** the vault: `~/Documents/Second Brain/`, with a minimal
`.obsidian/app.json` so Obsidian can open it (File → Open folder as vault). Had
a vault existed, the second brain would have been a `Second Brain/` subfolder
inside it, adding to your notes and never moving them.

`EVE_VAULT` points it somewhere else, an existing vault included: the
skeleton only ever adds files and never overwrites one.

## The layout (PARA+)

| Folder | What goes in it |
|---|---|
| `00-vision/` | `README.md`: mission, identity, principles, 10-year vision, this year, this quarter. Every plan starts here. |
| `01-inbox/` | Quick capture. One file per text, frontmatter `created` / `source` / `kind` / `status: inbox`. Fills up; the triage workflow empties it. |
| `02-todos/` | `master.md`, the one list: `## P0` (today or else), `## P1` (this week), `## P2` (soon), `## P3` (someday). |
| `03-projects/` | Active projects with an end, ideally fifteen or fewer. A folder or a file each; `status: done` or `archived` in its frontmatter takes it off the active list. |
| `04-areas/` | Ongoing areas of life: health, money, home, family, craft. |
| `05-resources/` | Reference. |
| `06-processes/` | The four SOPs (`daily-plan.md`, …) and their context packs (`packs/*.pack`). These *are* the prompts. Edit them and the workflows change. |
| `07-events/` | Meeting and event notes. |
| `08-journals/` | `daily/YYYY-MM-DD.md`, one page a day of timestamped lines; reflections and brain dumps. |
| `09-archive/` | Done and out of sight, with the folder structure above kept underneath. |
| `CLAUDE.md` | The map: what is in each folder, how things get in, how the workflows read it. Written for an agent reading the vault cold. |

## Capturing from Telegram

Capture is code, not a model: a handful of prefix rules (`classify_capture`)
decide what a text is, and the file is written before eve answers. A ✍
reaction on your message confirms it (owner, 2026-09-23). Only if the reaction
fails does eve send one line naming the file instead.

| You text | What happens |
|---|---|
| `note: the pasta place on Castro is Doppio` | `01-inbox/2026-09-21-1830-the-pasta-place-on-castro-is.md` |
| `remember this: Sam prefers mornings for calls` | same: an inbox note (a permanent fact about *you* is still `remember`) |
| `idea: a robot that waters plants` | an inbox note with `kind: idea` |
| `todo: buy milk` / `task: …` / `- [ ] …` | `- [ ] buy milk (added 2026-09-21)` under `## P2` of `02-todos/master.md` |
| `todo: p0 renew passport` / `p1 book the dentist` | the same line under `## P0` / `## P1` (`p3 ` for someday); the prefix is stripped |
| `journal: felt sharp this morning` / `today I …` / `dear diary …` | `- 18:30 felt sharp this morning` appended to `08-journals/daily/2026-09-21.md` |
| anything else eve is asked to keep | an inbox note |

Then: "what did I note about the pasta place" searches and reads; "what's on
my list" is `list_todos`; "what's in my inbox" is `list_inbox`; "file that
under the garden area" is `file_note` into `04-areas/garden`. File names are
`YYYY-MM-DD-HHMM-<slug>` with a slug of at most six words and 48 characters,
so the inbox sorts chronologically in any tool. A new capture with a name clash
gets `-2`, `-3`. Existing notes can be edited in place with a saved previous
version; files are never permanently deleted. Archiving is a move into
`09-archive/`.

## From the app

Eve Link's **Notes** (Apps tab, docs/app-chat.md) shows this vault as a small
native Notion: folders, pages with tickable to-dos, an editor, New, Move,
Delete with Undo, and search (`link_notes.py`). These are your edits, not
eve's: a save replaces the page's body (its frontmatter stays) and keeps the
previous version for **Undo last change**; a save over a version that changed
on the Mac since is refused. A delete moves the note into the hidden
`.eve-trash/<id>/` beside `.eve-history/`, out of search and context packs,
so Undo can put it back (`restore_note`); nothing is unlinked. Empty the trash
by hand when you want the files gone.

## Changing an existing note

You can text changes to the same note without creating replacement copies:

| You text | What eve can do |
|---|---|
| “Add alcohol wipes to my shopping list” | Find and read the list, then append the item to that file. |
| “Change bathroom mat to a blue bathroom mat” | Replace that passage while preserving the other items. |
| “Remove the curtains from my shopping list” | Remove the matching passage. The previous version remains available for undo. |
| “Mark buy milk done” / “Reopen buy milk” | Change the item's checkbox in the todo list. |
| “Move buy milk to P1” | Move that item between priority sections in one edit. |
| “Undo that edit” | Restore the previous contents, provided no later change conflicts with the edit. |

The text brain is instructed to use `search_notes` → `read_note` → `edit_note`
for changes to an existing note. If several notes could be the requested list,
it asks which one. It must not create a replacement copy or archive another
note to simulate an edit. Natural-language interpretation is model driven;
the tools enforce the file and version checks.

`read_note` returns a revision of the complete file, even when the displayed
text is clipped. `edit_note` requires that revision and replaces one exact,
unique body passage; an empty old passage appends, and an empty replacement
removes the matched passage. Frontmatter stays intact. A changed revision or
an ambiguous match is refused, so eve must read again. Telegram vault tool
operations are serialized; writes replace a complete file atomically.

Before each edit, the previous contents are saved privately under
`.eve-history/` inside the vault. Search and workflow packs exclude that
folder. `read_note` returns the available `undo_id`; `undo_note` uses it and
the current revision to restore the previous contents. Successive undo calls
walk back saved edits. Undo refuses to replace a later manual edit. These
controls cover edits, not captures or filing moves; history is tied to the
note's vault path, so moving or renaming the file makes its old undo history
unavailable at the new path. Existing duplicate notes are not merged automatically.

## The four workflows

Each is an SOP in `06-processes/<name>.md` plus a context pack in
`06-processes/packs/<name>.pack`. `workflow_prompt` compiles them into one
string: the SOP, today's date and the active project list, the selected files
inside `<context>`, and whatever you supplied. It makes **no model call**; the
text brain hands the prompt to `think_hard` and texts you the answer, except for
the daily plan, below.

**Today's plan is the rundown.** On Telegram, "plan my day", "what's my plan
today" (typo "plane" included), "what's on today", "my schedule today", "what
do I have today" and close variants take the rundown path
(`rundown.matches`): one brain reads today's calendar and mail through your
connected apps plus the vault's open checkboxes, and ends with a short plan
around the day's events. It came from a real miss on 2026-09-24 08:44: "What is
my plane today" went vault → `think_hard` (20.5 s, high effort), 39.2 s and
three model calls in all, and came back "a suggested plan… with no calendar
checked", because the `daily-plan` pack has no calendar and the second model
had no tools to read one. Where `daily-plan` still runs (a phrasing the rundown
does not catch), the tool result's `note` and a line added to the prompt tell
the text brain to read today's calendar and write the plan itself in the same
turn; it never goes to `think_hard`. The other three workflows still do.

| Say | Workflow | Reads | Gives back |
|---|---|---|---|
| "plan my day" | `daily-plan` (on Telegram: the rundown) | vision, todos, the last three journal days, five most recently touched project notes, plus today's calendar read by the text brain | the one most important thing, up to three more, time blocks around the day's events, one line of notice |
| "weekly review" | `weekly-review` | todos (ticked ones included), vision, eight journal days, projects, recent events | wins, misses, learnings, next week's P0/P1, stale projects |
| "triage my inbox" | `triage-inbox` | every inbox note, the project and area names | JSON decisions: where each note goes, todos to add, questions, texted back to you. Nothing moves until you ask; then the text brain files each note you name with `file_note` (never deletes) |
| "distill this" + a pasted transcript | `distill-chat` | the transcript, the project and area names | title, gist, decisions, action items, insights, open questions, where it belongs |

## Context packs

A pack says which files a workflow may see, in what order, within a token
budget (tokens ≈ characters / 4). "Life as a spec": the agent compiles its
context from the vault rather than being told about your life in a prompt.

```yaml
name: daily-plan
description: What today needs.
budget_tokens: 6000
include:
  - 00-vision/README.md                 # a bare path: the file itself
  - 02-todos/master.md
  - path: 08-journals/daily/*.md        # a glob, ordered, cut to a count
    select: filename_desc               # or modified_desc
    count: 3
  - path: 03-projects/**/*.md
    select: modified_desc
    count: 5
exclude:
  - 09-archive/**
rules:
  prefer_files:                         # these go first and are dropped last
    - 02-todos/master.md
  max_per_glob: 10
```

`compile_pack` resolves the globs, puts preferred files first, drops files
from the tail until the estimate fits the budget, reports what it dropped, and
emits:

```xml
<context pack="daily-plan" compiled="2026-09-21">
<file path="02-todos/master.md">
…
</file>
<file path="00-vision/README.md">
…
</file>
</context>
```

The parser is a few dozen lines for exactly this shape: scalars, lists, lists
of flat mappings and one level of nesting. It is not YAML and it does not
pull in PyYAML. `.obsidian/` and hidden paths are excluded from every pack.

## Knobs

| Variable | Default | What it does |
|---|---|---|
| `EVE_SECOND_BRAIN` | `0` | The switch. On, the text brain gets `capture_note`, `search_notes`, `read_note`, `edit_note`, `undo_note`, `list_inbox`, `file_note`, `list_todos`, `second_brain_workflow`. |
| `EVE_VAULT` | `~/Documents/Second Brain` | The vault root. Point it at an existing Obsidian vault (or a subfolder of one) to keep everything in one place; only files are added. |

## Storage and model access

The vault and edit history are folders on disk. The tools perform local file
operations without making model calls themselves. Their results go back to
the Telegram text model: searches include snippets, reads include note text,
and workflow prompts include the files selected by the pack within its budget.
Edit and undo results carry paths, revisions and undo IDs, not the saved history
contents. Logs carry counts and file names, never note bodies.

`read_note`, `edit_note`, `undo_note` and `file_note` refuse source paths that resolve outside the vault or
into `.obsidian/`. Note contents come back to the text brain as your words,
not as instructions, and its prompt says so.

## Credits

The design is Patrick Ellis's "The AI Second Brain: Building Your Personal OS
for the Age of Agents" (Feb 2026): the PARA+ layout, zero-friction capture,
vault-driven agent workflows, and context packs. PARA (Projects, Areas,
Resources, Archive) is Tiago Forte's.
