# Repos

Ask eve about the code in your own repositories, and follow up.

Owner, 2026-11-09: *"allow me to ask direct questions about my repositories through
eve ... like i should be able to follow up and such"*.

## What it does

"How does the composer get cleared after I send a message in eve?" goes to
`repos_ask`. eve runs Claude Code headless in that repository's folder on the Mac,
with tools that only look, and passes its answer back in the chat. "Which file is that
test in?" right after continues the **same Claude Code session**, so it already knows
what it read for the first question.

Measured on this Mac (2026-11-09, Sonnet): a first question 18 to 26 s, a follow-up
about 9 s.

## Threads

- One thread per repository. The first question starts a Claude Code session there
  (`claude -p --session-id <new id>`); the next question about the same repository
  resumes it (`--resume <id>`).
- A thread idle for two hours ends; the next question starts a new one. The model
  sets `new_thread` when you change topic in the same repository.
- Threads are kept in `~/.config/eve-bridge/repo-threads.json` (mode 600: the session
  ids and when each was last used), so a daemon restart keeps them.
- If Claude Code no longer has the session, the question starts a new thread.

## Read-only

The Claude Code session gets:

- `Read`, `Grep` and `Glob`;
- `Bash`, for git commands that only look: `git log`, `show`, `diff`, `status`,
  `blame`, `ls-files`, `shortlog`, `rev-parse`, `branch --list/-a/-v`, `tag --list`,
  `remote -v`, `describe`. A git command with `--output` (which writes a file) is
  refused.

It runs in `dontAsk` mode, so anything else is refused, never asked. It loads no MCP
server (`--strict-mcp-config`), and only the repository's own settings
(`--setting-sources project,local`): your own hooks do not run for it. It answers from
the checkout as it is on the Mac, with the branch it is on and any uncommitted work. It
never fetches.

## Which repositories

Every git repository (a folder with a `.git` folder; worktrees are skipped) up to three
folders below a root. The roots are `EVE_REPOS_ROOTS`, `:`-separated; the default is
`~/Documents:~/Developer:~/code:~/src:~/Projects`. A name matches the folder's name,
ignoring case and punctuation ("spotify knob" finds `Spotify_Knob`), or a unique part
of it. When two repositories share a name, use the path below the root
(`work/portfolio`); eve asks you which when it cannot tell.

## Settings

| Name | Default | What it does |
|---|---|---|
| `EVE_REPOS` | off | `1` turns the app on. |
| `EVE_REPOS_ROOTS` | see above | Where to look for repositories. |
| `EVE_REPOS_MODEL` | `sonnet` | The Claude Code model for the answers. |
| `EVE_REPOS_API_KEY` | unset | A key for these Claude Code runs. Unset, they use your own Claude Code login. eve's `ANTHROPIC_API_KEY` is never lent to them. |
| `EVE_CLAUDE_BIN` | `claude` on the PATH, else `~/.local/bin/claude` | Claude Code's binary. |

Restart the daemon after changing any of them.

## Tools

| Tool | Acts? | Approval (`policy`) | What it does |
|---|---|---|---|
| `repos_ask` | no | read (outside text: its answer taints the turn) | One question to the repository's thread. Answers `{ok, repo, thread: new or continued, question_in_thread, text}`, the answer cut at 8,000 characters. A run is stopped after 180 s. |
| `repos_list` | no | read (clean) | The repositories it can read, most recently used first. |

Both are left out of the voice: an answer takes ten seconds to a minute.

## The phone

A status-only sheet: how many repositories it found.

## What leaves the Mac

The question and the code Claude Code reads go to Anthropic, as in any Claude Code
session. The answer goes into the turn, so it reaches the chat model like any tool
result. The daemon's log has the repository's name, the time taken and counts, never
the question or the answer.

## Measured

The routing eval (blind, pre-registered, [bar](evals/repos-routing-2026-11-09/BAR.md))
**did not pass** in two rounds, so the app is off by default. With it on, questions about
how or where something is in the code reach `repos_ask` in most cases (round 2: 15 of 24
repos cases, against 1 of 24 without the app); "why was it built this way" and "what
changed this week" about a project the debrief also knows often go to the debrief
instead, which answers from your notes rather than the code. Saying "the code" or "the
repo" helps. Verdicts: [round 1](evals/repos-routing-2026-11-09/round1/VERDICT.md),
[round 2](evals/repos-routing-2026-11-09/round2/VERDICT.md).

## Tests

`bridge/tests/test_repos.py`, with a stand-in for `claude`: the read-only flags, the
thread (new, continued, a new topic, idle, a restart, a lost session), the names, eve's
key not lent, failures as reasons, the sheet, the registry line.
