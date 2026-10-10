# Browser delivery verification, 2026-09-23

The daemon log at `~/Library/Logs/eve-bridge.log` shows Telegram sends and
polls succeeding around 02:35–02:43. It deliberately omits message bodies; Eve
stores distilled conversation notes rather than raw chat transcripts. The exact
historical site prompt could not be recovered from those logs.

The source confirmed that task pictures came from the desktop screen after the
agent returned, independent of the browser tab it used. This explains how an
unrelated window could reach Telegram.

A fresh real Codex app-server task used Chrome to open `https://example.com`.
The installed runtime emitted an empty `cua_repl` form with connector
`browser-use`, tool `access_browser_origin`, and origin `https://example.com`.
With `site_access=allow`, Eve returned `accept` without asking the owner, saving
a grant, or fabricating an automated-review result. The task observed the
Example Domain heading and emitted a JPEG from tab `229895500` together with
that tab's fresh accessibility state. Eve accepted the matching capture.
The JPEG was visually inspected: it shows Example Domain, not the desktop.
The production Telegram `_send_screen` path then delivered these exact bytes
to the configured owner and returned `source: task_browser`.
The owner confirmed in this task: "browser tab looks good" and "Just saw in telegram".
The local preference `EVE_CODEX_SITE_ACCESS=allow` was enabled, Eve was
restarted, and launchd reported PID 28282 running with Telegram listening again
at 02:53:18. The package imports this workspace's edited source.

A separate built-in-browser probe returned `Browser is not available: iab`.
This is a runtime limitation of Eve's standalone session. The prompt defaults
unspecified browser requests to Chrome; explicit built-in-browser requests remain
explicit failures. No other browser was silently substituted in that probe.

Folder selection was checked against the live read-only Codex task catalog:
`eve` resolved to `/Users/gurucharan/Documents/personal/eve`. Unit tests
cover latest-task choice, case-insensitive names, spaces, full paths, ambiguous
duplicate folder names, unknown folders, and the absence of a numbered menu.

The first regression run exposed an existing timing race in the second-brain
test: a fixed number of event-loop yields could finish before filesystem work.
That test now awaits the actual turn. See GATES.md for final executed checks.

## Fresh folder chats replace task selection

The owner supplied the actual Telegram text. It confirms that the `eve` menu
entry was internal task `01a0cdb1-92f2-7692-ab40-ca73f67eaa6d`, followed by an
owner-discovery timeout and ordinary messages stuck in disconnected relay mode.
The read-only catalog identifies its source as `{"subagent":{"other":"guardian"}}`,
updated at 02:56:02, with no agent nickname. Filtering only nicknames was wrong.
The legacy catalog now excludes structured subagent sources; its regression test
has a positive control showing that the old query selects the hidden task.

The owner then changed the desired behavior: list accessible folders only and
start a new chat. Telegram now uses `codex_chat.py` and public app-server sessions,
not the legacy desktop follower. Saved local project roots are the folder source.
Each selection starts a new persistent thread; follow-ups keep its context.
Startup errors return to Eve without replaying any work message. Computer task
headings now say `Task result`, avoiding a success claim for a textual refusal.

A live check used the production `TelegramInlet._dispatch` and `CodexChat` with
the installed Codex binary. Its Telegram receiver captured messages locally;
the test did not send model-generated test messages to the owner. `codex on`
returned exactly the accessible-folder list. `codex eve` created fresh thread
`01a0cdbb-e631-7993-8cbb-2fe4ec544ebf` with the actual Eve cwd. Two harmless turns
returned `READY` and then the token `cobalt-pine-73`, demonstrating continuity.
The test thread was archived through the public protocol, and `codex off` closed
the session. The probe printed `FRESH_CODEX_CHAT_LIVE_OK` and exited zero.

After deployment, launchd reported the daemon running as PID 33006, exit code 0;
the log confirms Telegram listening at 03:09:35. The documentation checker printed
`TELEGRAM_DOCS_OK`. Full raw historical Telegram texts remain unavailable from
the daemon logs; the supplied chat export is the evidence for those messages.
The folder-only list was also delivered to the configured Telegram owner through
the real Bot API, printing `TELEGRAM_FOLDER_ONLY_LIST_DELIVERED` after success.
