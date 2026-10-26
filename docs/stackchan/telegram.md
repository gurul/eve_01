# Text eve through Telegram

Until this, the only way words reached eve was the microphone: a conversation
needs a live mic, so away from the desk there was no eve at all. `telegram.py`
is a text door. From your phone you can chat with eve, start a task on the
Mac, stop it, answer the question a task asks before it does something
consequential, and get a photo from the robot's camera.

It ships **off**. No evaluation of the text brain exists yet, so
`TELEGRAM_DEFAULT` is `False` and a test holds it there.

```
your phone ── Telegram ──▶ api.telegram.org ◀── long poll (outbound HTTPS) ── telegram.py in the daemon
                                                                                   │ accept(): owner id, private chat,
                                                                                   │ fresh, your own words — or dropped
                                                                                   ▼
                                                        text brain: gpt-6-luna, Responses API, store=False 
                                                          tools: start_task · steer_task · stop_task · take_photo ·
                                                                 screenshot · send_file · list_files · look · look_around ·
                                                                 find · move_head · go_explore · set_sound · take_notes ·
                                                                 remember · think_hard · web_search (+ memory_search · memory_read ·
                                                                 forget_preview · forget_apply with EVE_MEMORY=1)
                                                                                   │ start_task
                                                                                   ▼
                                              codex_computer.py → Codex app-server → installed cua_repl.js
```

There is no port to open, no webhook and no public URL. The daemon calls
Telegram; nothing calls the Mac.

The text brain runs on OpenRouter with `OPENROUTER_API_KEY` (sent as
`openai/gpt-6-luna`); the door stays shut without that key. Since 2026-10-08 eve
calls every model with this one key ([one key: OpenRouter](../openrouter.md));
`OPENAI_API_KEY` is not needed for the chat.

Computer tasks now use Codex's existing Computer Use through the public app-server
protocol. `start_task`, `steer_task`, and `stop_task` keep their existing interface.
Codex commentary appears as steps in the task's progress message, and app permission requests wait
for an explicit reply. Native app-access prompts offer `yes` (once),
`allow for task`, or `always allow` when Codex permits those choices; `no` denies.
`always allow` saves the app in Codex's own Always-allowed apps list for future
tasks, including after Eve restarts. Revoke saved grants in Codex desktop's
**Settings → Computer Use → Always-allowed apps**. Eve does not keep a second
allowlist. Managed restrictions, app-risk warnings and sensitive-action approvals
remain in force; audio and unknown request types cannot gain an app-wide grant.
A timeout, unsupported scope or unrecognized answer does not approve. Unknown
permission forms are declined with an explanation.
The adapter never falls back to Eve's old desktop worker. See
[the verified integration and limits](../codex-computer-use/README.md).

What else the Bot API offers, and how eve could use it: [telegram-bot-api.md](telegram-bot-api.md).

## Rundown

Text `rundown`, `/rundown`, or `eve: rundown` for today's **email, calendar,
Slack, and Obsidian todos**. Asking for today's plan is the same command: "plan my
day", "what's my plan today" (or "plane"), "what's on today", "my schedule today",
"what do I have today", "what does my day look like" and close variants, matched
whole-text by `rundown.matches`; then the reply ends with a short plan around the
day's events. "Plan a trip" and "today's news" stay ordinary turns. This command
addresses Eve even while either relay is selected. It loads the packaged `skills/rundown/SKILL.md` each time and obtains
fresh app data: email and calendar through the direct apps ([apps.md](../apps.md)), Slack
through the existing Composio session. The skill's first step is written for the turn: it names
the tools only of the sources that are present, and says the others are not connected. Email includes today's inbox
and older unread items needing attention; Slack focuses on mentions, DMs, and
recent requests. Calendar queries use local midnight through the next midnight.

Todos come from open markdown checkboxes in `EVE_VAULT` (the existing
Second Brain/Obsidian vault). The bridge, not the model, groups each open item as
**today**, **overdue**, **upcoming** or **undated** (`rundown.task_date`), and
hands the model an ISO `date` on every dated item:

- Obsidian Tasks markers (`📅`, `⏳`, `🛫`, `due::`, `scheduled::`, `start::`) win
  when present.
- Otherwise a date written anywhere in the line counts: `Oct 22`, `October 22nd`,
  `Oct. 22`, `22 Oct`, `22nd of October`, `Oct 22, 2027`, `2026-10-22`,
  `10/22/2026`, `10/22/27`. Numbers are month/day. A yearless `10/22` counts only
  at the start of the line or after on/by/due/before/until, so `1/2 gallon` stays
  a quantity. A lowercase `may` is a verb, not the month.
- A date with no year is its occurrence nearest today (a tie goes forward): up to
  about six months back it is overdue, up to about six months ahead it is upcoming.
- The earliest date in a line wins. Invalid dates such as `Feb 30` are ignored.
- The date an item was written down or finished is not a due date: `(added
  YYYY-MM-DD)` from capture, `created::`/`done::` and the `➕`/`✅`/`❌` stamps are
  removed before the dates are read.
- An open item in today's daily note with no date of its own is due today.

Upcoming and overdue items are sorted by date. Completed items, hidden folders,
archives and symlinks are excluded. (Until 2026-09-30 only Tasks markers were
read, so "Oct 22: go to Mount Tam" was reported as undated backlog, and every
future item was dropped.) The scan is bounded to 5,000 files, 1 MiB per file and
200 tasks. `EVE_SECOND_BRAIN=1` supplies the vault; the direct apps supply Gmail and
Calendar, and Composio supplies Slack. A source that is not connected, or whose read failed
outright, is named rather than treated as empty. Result limits, pagination and
clipping are never mentioned: the rundown reports what it retrieved (owner,
2026-09-23). Rundown exposes only read/search tools and rejects mutations before execution.
It does not send messages, mark mail read, change calendar events or edit todos.
With [Canvas](canvas.md) set up, unsubmitted Canvas work due in the next three days
is read by code before the turn and listed under Todos.
The retrieved content is summarized by Eve's configured text model (an OpenAI model, through OpenRouter).

## Receiving images

Send a Telegram photo or an image file, optionally with a caption. Still JPEG,
PNG, WebP and GIF are accepted, up to 10 MB and 25 megapixels. Each photo in an
album is delivered separately. Voice notes, animations and other documents are
not accepted. Owner/private-chat, freshness and forwarded-message checks apply
before downloading; bytes and decoded dimensions are validated before use.

With `claude on` or a selected Codex folder chat, the caption and a local image path
are sent to that same session with an instruction to read the image. The image
is not silently routed to Eve. This uses the existing text relay and the
recipient's image-reading tool, with its normal file permissions; it does not
add a private attachment endpoint. Claude's official
[image workflow](https://code.claude.com/docs/en/common-workflows#work-with-images)
documents passing a local image path. Codex can inspect it using `view_image`.
These relays target sessions running on this Mac; remote sessions cannot read
its temporary files. `eve: <caption>` addresses Eve instead.

A relayed image gets a 👀 reaction when it arrives and a 👍 in its place once it
reached Claude or Codex (owner, 2026-09-23). If it is not delivered, the 👀 comes
off and a line says why. If reactions fail, the image still goes, and the usual
line (`Typed.`) says so; a 👀 whose 👍 could not be set is taken off, so it
never still says "on its way".

Without a relay, image bytes go to Eve's configured model (through OpenRouter) as
an `input_image` data URL. The Telegram download URL/token never goes to the
model. Image bytes are not included in Eve's text conversation history; a
later independent Eve turn needs the image resent. An image caption cannot
answer a pending permission question or run chat commands such as `stop`.
Answer the question in a separate text and resend the image afterward.

Relay images are stored in a private `eve-telegram-images-<uid>` directory
under the Mac's temporary directory, with owner-only permissions. Files older
than 24 hours are removed on the next image save, with a 100-file limit. They
remain available long enough for queued turns to read them. Relay confirmation
means the message was submitted, not that the recipient has inspected it.

## Set it up

1. In Telegram, talk to **@BotFather**: `/newbot`, pick a name, copy the token.
2. Put the token in `~/.config/eve-bridge/env` (mode 600):
   ```
   EVE_TELEGRAM_TOKEN=123456:AA…
   ```
3. Send your new bot any message from your phone, then find your numeric id:
   ```
   eve-bridge telegram-check
   ```
   It checks the token and prints `user id <number> (<first name>)` for whoever
   has messaged the bot. It prints ids and first names only, never what anyone
   wrote, and it reads without acknowledging, so the daemon still sees those
   messages later. Run it while the daemon's door is off: Telegram allows one
   poller per bot and answers a second with `409`.
4. Add your id and the switch, then restart the daemon:
   ```
   EVE_TELEGRAM_OWNER=<your id>
   EVE_TELEGRAM=1
   ```
   The log says `telegram: listening for 1 owner id(s)`.

All three are required. The switch without a token is off; the switch and a
token without an owner id is off. A door with no allowlist never opens.

| Variable | Default | What it does |
|---|---|---|
| `EVE_TELEGRAM` | `0` | The switch. `app` keeps the brain for the Eve Link app, calls, the watcher and the chief, but never polls Telegram and sends nothing to it (`MutedBotApi`): eve's lines reach the owner through the transcript, which the app shows. |
| `EVE_TELEGRAM_TOKEN` | unset | The bot token from @BotFather. Never logged. |
| `EVE_TELEGRAM_OWNER` | unset | Numeric user ids allowed to text eve, comma-separated. A `@username` is ignored: it can be changed and re-registered, a number cannot. |
| `EVE_TELEGRAM_MODEL` | `gpt-6-luna` | The text brain. Chosen 2026-09-24 over `gpt-6-astra` for speed: a tool turn on astra took 39 s. |
| `EVE_TELEGRAM_ASK` | `0` | `1`: with the Claude relay on, every tool call is asked in the chat, not only the always-ask ones (never in bypass mode). |
| `EVE_TELEGRAM_DRAFTS` | `1` | `0`: while Claude works on a relayed line, show "typing…" instead of the "Thinking…" bubble. |
| `EVE_TELEGRAM_EFFORT` | `low` | Its reasoning effort (`low`, `medium`, `high`, `xhigh`, `max`). Hard questions go to `think_hard` instead. |
| `EVE_APP_THINK_MODEL` | `gpt-6.1-sol` | The model for an Eve Link app turn sent with the speed dial on **Think** (`"grade": "think"`). The whole turn runs on it. See [Chat in the app](../action-button.md#chat-in-the-app). |
| `EVE_APP_THINK_EFFORT` | `medium` | Its reasoning effort (`low`, `medium`, `high`, `xhigh`, `max`). Any other value is logged and `medium` is used. |
| `EVE_GRADE_ROUTER` | `auto` | Whether Jev grades an Eve Link app turn on the speed dial's **Auto** (`"grade": "auto"`) as `think` or `instant` ([Chat in the app](../action-button.md#chat-in-the-app)). Graded `think`, the turn runs on `EVE_APP_THINK_MODEL` at `EVE_APP_THINK_EFFORT`; graded `instant`, it is the chat as it is. `auto` asks Jev (it passed its blind holdout) when a Jev route is set; `on` asks Jev regardless of the holdout; `off` never asks, and every Auto turn is the chat as it is. A Jev error or a timeout (0.8 s) grades `instant`. Telegram turns are never graded. While it is on, your app message and the one before it go to Jev on every Auto app turn. |
| `EVE_WEB_SEARCH` | `openrouter-perplexity` with an OpenRouter key, else `off` | How every brain searches the web ([below](#web-search)): `openrouter-perplexity`, `openrouter-exa`, `openai` (OpenAI's hosted tool, explicit only), `off`. |
| `EVE_WEB_SEARCH_MODEL` | `openai/gpt-6-luna` | The OpenRouter model that writes the answer from the search results. Any OpenRouter id, `openai/…` included. |
| `EVE_WEB_SEARCH_RESULTS` | `5` | Results per search, 1 to 10 (the engines' first price tier). |
| `EVE_WEB_SEARCH_MAX_USES` | `2` | Searches per question, 1 to 5. |
| `EVE_APPS` | `0` | `1`: Gmail, Calendar, Drive (Google's APIs), GitHub and Maps (their MCP servers) directly, each once signed in. Settings and sign-in: [apps.md](../apps.md). |
| `EVE_APPS_POLICY` | `gmail=ask,calendar=write,drive=ask,contacts=read,github=ask,maps=read` | What a changing call to a direct app may do ([apps.md](../apps.md#settings)). |
| `EVE_APPS_TIMEOUT_SECS` | `60` | One direct app call's timeout. |
| `EVE_APPS_DIR` | `~/.config/eve-bridge` | Where the Google OAuth client and token files are kept. |
| `EVE_GITHUB_MCP_TOKEN` | unset | GitHub's fine-grained token for its MCP server. |
| `EVE_GOOGLE_MAPS_KEY` | unset | The Maps Grounding Lite API key. |
| `EVE_COMPOSIO` | `0` | `1`: Slack, Notion, Linear and the long tail through Composio ([below](#the-apps-composio)). Needs `COMPOSIO_API_KEY` in the env file. |
| `EVE_CODEX_BIN` | desktop app bundled Codex, then `codex` on PATH | Executable for computer-task delegation. Computer Use must be enabled in its installed plugins. |
| `EVE_COMPOSIO_POLICY` | `gmail=read,googlecalendar=auto,googledrive=auto` | `read` refuses writes; `auto` and `write` run audited routine operations and ask for consequential or unknown writes; `ask` asks for every write. Unlisted toolkits use `auto`. Reads always run. |
| `EVE_COMPOSIO_TOOLKITS` | Slack, Notion, Linear, plus every app you have connected | The apps eve's Composio session may use (an allow-list, comma-separated). `all` removes the allow-list but still keeps out the apps below. Web search and scraping toolkits are never used either way (eve's own `web_search` covers the public web), and neither are Gmail, Google Calendar, Google Drive and GitHub, which go direct (`DIRECT_TOOLKITS`, [apps.md](../apps.md)). |
| `EVE_COMPOSIO_STATE` | `~/.config/eve-bridge/composio.json` | Where the session id is kept between restarts. |
| `EVE_COMPOSIO_TIMEOUT_SECS` | `60` | One app call's timeout. |
| `CANVAS_BASE_URL` | unset | Your school's Canvas, `https://<school>.instructure.com` or its own domain; https only ([canvas.md](canvas.md)). |
| `CANVAS_API_TOKEN` | unset | A Canvas access token (Account → Settings → New Access Token). With both set, the brain can read Canvas; with either unset, it has no Canvas tools. `EVE_CANVAS=0` turns it off. |
| `EVE_SHARE` | `0` | `1`: `share_file` and `find_contact`, sending one of your files to someone by iMessage (this Mac) or by email as a Google Drive link (the direct apps' Google sign-in), after your Allow ([share.md](../share.md)). |
| `EVE_TOOLSET_ROUTER` | `auto` | Which toolsets a turn starts with ([toolset-router.md](../toolset-router.md)): the direct apps, Share and the workspace (Slack, Notion, Linear through Composio) open by the message's words plus Jev's judgment of it; `open_app` opens the rest, and a call to a closed toolset's tool opens it and runs. `auto` asks Jev (it has passed its blind holdout) when a Jev key is set, else keywords only; `on` asks Jev regardless of the holdout; `off` is the turn as it was before the router: every Share and Composio tool on every turn, the direct apps by their words. While Jev is asked, your message and the one before it go to Jev on every turn. |
| `EVE_MENU_TOOLS` | `menu_tools.SHIPPED` (`True` since the v4 blind tool-choice eval passed, 2026-10-09) | The / menu as tools, `events_search`, and the tool-choice lines (see "The menu in plain words" below); `off` sends every request byte for byte as before. The taint gate is on either way. |
| `EVE_GATE_WITHDRAW` | `0` | `1`: a message you type while the chat's own check-first or app question waits withdraws that question: the act does not run, and the same turn goes on with your words ([below](#a-typed-correction-withdraws-the-question)). Unset or `0` keeps today's behaviour byte for byte. Off until a blind eval of corrections against today's queue passes, because the withdrawn call's result is new text the brain reads. |
| `EVE_CHAT_LOG` | `1` | `0` turns the chat log off ([below](#the-chats-history-the-chat-log)): the history is then RAM only and is cleared after ten quiet minutes or a restart. |
| `EVE_CHAT_LOG_FILE` | `~/.config/eve-bridge/chat/log.json` | Where the chat log is kept. |
| `EVE_CHAT_LOG_LINES` | `300` | Lines kept per chat, at most. |
| `EVE_CHAT_LOG_DAYS` | `7` | Days a line is kept, at most. |
| `EVE_SECOND_BRAIN` | `0` | `1`: your own notes, todos and journals as a local markdown vault, captured from this chat ([second-brain.md](second-brain.md)). |
| `EVE_VAULT` | `~/Documents/Second Brain` | The vault's folder (open it in Obsidian). |
| `EVE_COMMAND_RISK` | `shadow` | The Auto Mode gate behind the Claude relay ([below](#the-auto-mode-gate-jev-judges-a-relayed-command)): `off`, `shadow` (judged and logged, never acted on), `ask` (a risky or failed verdict is your yes/no; a failed one reads `[Jev: unavailable]`). |
| `EVE_CODE_ROOT` | `~/Documents` | Where `new claude` looks for project folders. |
| `EVE_CODE_AREAS` | `personal,work` | The first question's answers: folders under the root, comma separated. |
| `EVE_CLAUDE_TERMINAL` | `warp` if installed | `warp` or `terminal` (Terminal.app): where a new session opens. |

## Questions and permission prompts from Claude Code

With `claude on`, what Claude Code asks you reaches the chat in full, not just
as "Claude is waiting on you":

- **A question with choices** (AskUserQuestion) arrives with its options
  numbered: "Quit all? (1. Keep terminals / 2. Everything)". Tap an option's
  button, or reply with its number; either way the relay types the number into
  the dialog. One question gets buttons; several questions in one call are
  answered by number, one after another. The buttons stop working once you
  type to Claude, the turn ends, or the relay is switched. eve's PreToolUse
  hook covers `AskUserQuestion` for this, and it only relays the question. A
  hook never answers it.
- **A permission dialog for any tool** (Edit, Write, WebFetch, an MCP tool)
  becomes a yes/no in the chat, through the `PermissionRequest` hook. It comes
  with **Allow** (green) and **Deny** (red) buttons. A tap answers it, and so
  does a short typed reply that is only a yes or a no ("yes", "go ahead",
  "no, leave it"). Anything else you type while it waits goes to Claude as
  usual, and the prompt keeps waiting. That includes a sentence that starts
  with a yes-word, like "ok, also update the README": it is a message for
  Claude, not an Allow. So is "eve: yes": it goes to eve and allows
  nothing. This holds from the moment the prompt is sent. After the answer, or after 240 s with
  none, the message changes to say "Allowed.", "Denied." or that the dialog on
  the Mac decides, and the buttons go. If you don't answer, the dialog stays on
  the Mac as before. If Telegram refuses the buttons, the prompt is plain text
  and your next message is the answer, as before. With the relay off, the hook
  does nothing. Turning the relay off, or moving it to another session, while
  a prompt waits ends that prompt: the dialog on the Mac decides, the message
  says so, and your next message goes to eve, not to the old prompt.
- **How a reply is read** (`consent.py`). One rule covers every yes/no that
  gates an action: permission prompts, app actions and auto-browser approvals.
  It fails closed. A reply is a yes only if its first word is a yes-word
  ("yes", "y", "ok", "sure", "allow", "do it", "go ahead"…) *and* nothing in
  it takes that back, so "yeah no", "ok wait" and "sure, but don't" are not
  yeses. It is a no if its first word is a no-word. Anything else approves
  nothing, and a permission prompt goes back to the dialog on the Mac.
- The general "Claude is waiting on you" notice is skipped for 20 s after the
  question itself was sent, so you don't get it twice.

`eve-bridge install` registers both hooks. Claude Code sessions that were
already open pick them up when they restart.

## Start a coding session

`new claude` (also `start claude`, `claude new`, `/newclaude`) opens a new
terminal window on the Mac with a coding agent running in one of your project
folders. The steps are code, with no model call, and each one is a short text:

1. **Personal or Work?** Recent sessions are listed underneath as numbers, and
   a folder name works here too. Most of the time you already know the folder.
2. **General, or which folder?** `general` opens the area itself, a name opens
   that folder, and `list` shows the folders with a number for each. Numbered
   worktrees (`era-maker-213`) are folded under their base folder. Names are
   matched loosely, so `era maker` finds `era-maker`. If more than one folder
   matches, you get a numbered choice.

When the relay joins the session it just opened, your messages wait until
Claude Code reports that it has started (its SessionStart hook from that
folder), then about a second more. A message typed into a window that is still
starting is lost. If the hook never arrives, typing goes ahead after 30 s.

Every question comes with **tap buttons** under it (an inline keyboard):
`Personal` / `Work` and the recent sessions, then `List` / `General`, then one
button per folder. A tap does exactly what typing the button's text does, such
as `2. era-maker`, and the number picks. Only the latest question's buttons
work: a tap on an older one says "This button has expired." Typing still
works. If Telegram refuses inline buttons, the question comes with the old
one-time reply keyboard instead.

You can say everything in one message: `new claude work era hub api`. You can
also just ask in plain words ("open up era maker in work"), and the text brain
walks the same steps through its `start_coding_session` tool. `cancel` ends the
steps, and an unanswered question is dropped after five minutes.

**Which program starts** comes from the area, so eve never asks: personal runs
`claude --dangerously-skip-permissions` and work runs
`era-code claude --dangerously-skip-permissions`. To switch for one session, say
the other one explicitly: "…but in claude instead of era code", "eve in
era-code", "atlas with claude".

In Warp, eve rewrites one launch configuration,
`~/.warp/launch_configurations/eve-claude.yaml`, and opens it with
`warp://launch/eve-claude`. Warp finds a configuration by its `name:`, not
by its file path. With `EVE_CLAUDE_TERMINAL=terminal`, Terminal.app runs
`cd <folder> && <command>` instead. The folder is always one taken from the
listing, never text from the chat, and it is shell-quoted. After the window
opens, `claude on` connects the chat to it as usual.

`claude on` itself starts from the sessions that are running (the daemon knows
each one from its hooks). A session whose terminal was closed or crashed never
says goodbye, so before it lists them eve checks the Mac: it looks for a
running `claude` process in each session's folder (`ps` and `lsof`, about
30 ms, run off the daemon's event loop; a message you send while it runs
waits and is routed right after). An npm install started through its `claude`
shim (`node …/bin/claude`) counts as a `claude` process too. A session with
no process there is left out and forgotten. If that
check fails or finds no `claude` at all, every session is listed, as before.
A session that started in the last 30 s, or one with a question still waiting,
is always listed.

- **One session:** the chat joins it at once.
- **Several:** "Which Claude session?" with a button per folder
  (`eve`, `era-maker`) and `New claude`. A tap is the same as typing
  `claude on eve` or `new claude`. The chat then
  follows only the session you picked: another session's text stays on the Mac.
- **None:** it walks the steps above, opens the terminal, and joins that session
  when it opens.

`claude on <name>` picks a running session by folder name. If no running session
matches, it starts that folder.

## Choosing the text brain (measured)

On 2026-09-24 an [Ori](https://openrouter.ai) eval compared six models as the
Telegram text brain. It used 16 cases: 11 of the owner's real Telegram turns (a
few lightly redacted) and 5 authored tool cases. Tools were mocked, tool
choices were checked in code, and replies were judged by Claude Opus 5.5. The
eval called the models through OpenRouter's Chat Completions API; production
called OpenAI models directly then, and has called them through OpenRouter's Responses API since 2026-10-08.

Five short rules were added to the instructions from that eval: keep API keys
out of the chat, route Mac requests to `start_task`, check the calendar or ask
for an ambiguous "plan/plane today", give planning help a structure plus one
question, and own a missed date and offer to save it. With those rules:

| Model | Passed, before → after the rules | Cost for all 16 cases |
|---|---:|---:|
| `gpt-6-luna` (the default) | 11 → 14/16 | $0.0012 |
| `gpt-6-sol` | 13 → 14/16 | $0.021 |
| `gpt-6-astra` | 11 → 14/16 | $0.092 |
| `google/gemini-3.7-flash` | 10 → 15/16 | $0.061 |
| `z-ai/glm-5.3-flash` | 11 → 12/16 | $0.0056 |
| `anthropic/claude-opus-4.7` | 12 → 13/16 | $0.730 |

Luna stays the default: it matches astra with the rules at about 1/75th of the
cost. No model passed all 16. Luna still misses offering to save a date it
missed, and the sample is small, so rerun the eval before changing the model.

## Web search

Voice, Telegram and `think_hard` search with **Perplexity through OpenRouter**
whenever `OPENROUTER_API_KEY` is set (owner, 2026-09-24). On OpenRouter's search
benchmarks Perplexity led BrowseComp, HLE and WideSearch on quality, value and
speed with the model held fixed; on BrowseComp (2026-08-18) Claude Opus 5 scored
89.0% with Perplexity against 82.2% with Exa. It costs $0.005 a search against
Exa's $0.007.

How one search works: the brain calls the `web_search` function tool, and
eve makes one OpenRouter chat call carrying the `openrouter:web_search`
server tool on Perplexity, capped at 2 searches of 5 results. A cheap
model, `openai/gpt-6-luna` at low effort, writes a short answer with its sources.
(Until 2026-10-08 an `openai/…` answer model was refused here; every model call now
goes through OpenRouter, so it is allowed.)

The answer step is also given the local date and time. Search engines are not
a live clock: Exa could not say the time in Seattle (2026-09-21), and neither
could Perplexity (2026-09-24). So a clock or "today" question is answered from
eve's clock instead of a page.

Measured 2026-09-24 through eve's own `websearch.search`:

| Question | Time | Cost |
|---|---:|---:|
| "What time is it in Seattle right now?" (answered from the clock, correct) | 1.5 s | $0.0002 |
| "What time is it in Tokyo?" (converted from the clock, correct) | 4.9 s | $0.0056 |
| "Who won the most recent Formula 1 Grand Prix?" (correct, 5 sources) | 4.2 s | $0.0058 |

`EVE_WEB_SEARCH=openai` selects OpenAI's hosted search tool (an explicit
choice only; without an OpenRouter key there is no brain to search, and search is `off`). `openrouter-exa` keeps Exa
on the same server tool, and `off` disables search. Restart the daemon after
changing the setting so new voice sessions pick it up.

The hosted search can leave empty citation links in a text reply, such as
`([]())` (seen on 2026-09-24). Eve takes empty markdown links out of the
brain's text before the reply is kept or sent. A group of links with no labels
is removed with its parentheses. A link with no label is removed. A label with
no address stays as plain words. Real links, inline code and code blocks are
not changed.

## The apps: Composio

Since 2026-10-04, Gmail, Google Calendar, Google Drive, GitHub and Google Maps go
**direct** ([apps.md](../apps.md)), and Composio keeps **Slack, Notion, Linear**
and anything else you connect there. Drive and GitHub are kept out of the Composio
session even when connected (`composio_tools.DIRECT_TOOLKITS`). Since 2026-10-05,
Gmail and Google Calendar join it only for your **other** (work) account, and each
call names it ([apps.md](../apps.md#connected-accounts-direct-and-composio-for-the-rest)). What follows is
Composio's design, unchanged for the apps it keeps. Examples below that name
Gmail or the calendar date from before the split.

Owner's decision, 2026-09-21: your apps are reachable by API through
[Composio](https://composio.dev) in seconds, where a Mac task drives the screen
for minutes. `composio_tools.py` keeps one Composio
*session* per owner (its user id is `telegram-<your id>`, so the connected
accounts are yours, never a chat's; the session id is persisted and resumed on
restart) and lends the session's meta tools to the text brain beside eve's own:
`COMPOSIO_SEARCH_TOOLS` finds the right tool for a use case,
`COMPOSIO_MULTI_EXECUTE_TOOL` runs it, `COMPOSIO_MANAGE_CONNECTIONS` hands you a
sign-in link for an app you have not connected. The brain is told: a job inside
one of your apps is done there, not on the Mac.

**What may run, by code, before any call (`composio_tools.decide`):** a call that
only reads (`GMAIL_FETCH_EMAILS`, `GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS`,
`GOOGLEDRIVE_FIND_FILE`: a reading verb among the slug's words and no writing
verb) runs at once. **Gmail remains read only through Composio**; the owner's
own mail uses the direct Gmail tools. For other toolkits, `auto` runs the
known private routine operations whose arguments establish their effect.
Sending, deleting, purchasing, publishing, changing access and unknown writes
ask first. A private calendar edit must explicitly suppress guest delivery;
adding attendees still asks. An explicit `read` override refuses writes and
`ask` asks for every write. `write` keeps the same consequential-action guard
as `auto`. Mixed batches take the strictest decision. `EVE_COMPOSIO_POLICY`
sets these per-toolkit rules. Sandbox bash and uninspectable code remain guarded.


**The workbench** (Python in Composio's sandbox) runs without a question when its
code only computes (owner, 2026-09-30: "I don't want it to ask me permissions
regarding this"). eve reads the script before it runs, without running it
(`composio_tools.workbench_slugs`):

- **Runs without asking:** standard data modules (json, re, datetime, pandas,
  numpy…), files in the sandbox, and the read-only helpers `invoke_llm`,
  `web_search` and `smart_file_extract`.
- **Judged like any other call:** each `run_composio_tool("SLUG", …)` in the
  script. A read runs; `GMAIL_SEND_EMAIL` is refused; a Drive write asks, as
  "Run GOOGLEDRIVE_UPLOAD_FILE from a workbench script?".
- **Still asks:**
  - `upload_local_file` and `proxy_execute`;
  - any other import (requests, os, subprocess…);
  - `eval`/`exec`/`getattr`/`__import__`, any name or attribute starting with
    `_` (`__builtins__` reached `exec`, found by the Lean model, 2026-10-05);
  - a pickle or HDF loader (`pd.read_pickle`, `allow_pickle=`, `read_hdf`),
    pandas' `.eval`/`.query`, and `operator.attrgetter`/`methodcaller`;
  - anything that evaluates a string annotation as code (`typing.get_type_hints`,
    `ForwardRef`, `functools.singledispatch`), since a tool call can hide in
    one;
  - a pandas or numpy reader that can fetch a URL (`read_csv`, `read_json`,
    `read_html`, `genfromtxt`…), since the owner's data could leave in its
    query string (owner, 2026-10-18; it was eve lite only before);
  - a slug that is not written out as a string, or the helper passed around
    under another name;
  - code that does not parse.

**Two accounts on one app** (owner, 2026-10-02: a second Google Calendar). Ask
eve to connect another account. If it can't, make a Composio Connect Link with
`connected_accounts.link(<user id>, <auth config id>, alias="work",
allow_multiple=True)` and sign in with the other account. Name your own account
on that app `personal` (`connected_accounts.update(<connection id>, alias="personal")`).
When any app has two or more ACTIVE accounts, eve does the following at start:

- **The session is made in multi-account mode with explicit selection.** Every
  `COMPOSIO_MULTI_EXECUTE_TOOL` call for that app must name its `account`, and
  Composio refuses one that does not, instead of picking one silently. The
  saved state records the mode. A session made in the other mode is not
  resumed: a new one is created.
- **The brain gets a short note** naming each app's accounts. It reads from
  every account and says which result came from where. It creates, changes and
  deletes on `personal` unless you name another account.
- **A confirmation names the account it writes to,** for example "Run
  GOOGLECALENDAR_CREATE_EVENT on work with …?".

With one account per app, nothing changes: no mode, no note, and the same
session.

Proven live 2026-09-21 from this code path: Gmail, Google Calendar and Google
Drive connected; `GMAIL_FETCH_EMAILS`, `GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS`
and `GOOGLEDRIVE_FIND_FILE` each returned a real result with a Composio log id.
Install: `pip install -e ".[composio]"` (composio 0.22.0, composio-openai 0.22.0;
`composio-core` is deprecated). The key lives in the env file, never in source.

## Canvas

With `CANVAS_BASE_URL` and `CANVAS_API_TOKEN` set, the brain gets four read-only
tools from `canvas.py`: `canvas_courses`, `canvas_due` (what is due, submitted or
not, in your time zone), `canvas_announcements` and `canvas_find_link` (links to
assignments, pages, files and modules). Only GET requests can leave. Setup,
endpoints and privacy: [canvas.md](canvas.md).

## The second brain

Owner's decision, 2026-09-21: "the second brain system is for my personal notes
and things of that sort I'll text to Telegram". A text is a note in seconds, in a
local markdown vault Obsidian opens, organised PARA+, with agent workflows (plan
my day, weekly review, triage my inbox, distill this) and context packs compiled
from it. Ask to add to, edit, remove an item from, or check off an existing note
or list; `edit_note` updates the same file after a versioned read, and
`undo_note` can restore its previous contents. Ambiguous matches and changed
versions require another read or clarification. The whole design, the capture rules and the pack format are in
[second-brain.md](second-brain.md). Ships off behind `EVE_SECOND_BRAIN`.

## The Auto Mode gate: Jev judges a relayed command

While the Claude relay is on the daemon is bypass, and the regex list
(`matchers.py`) was the only thing that could stop a Bash command; on the owner's
Mac that list is empty (`matchers.toml`, 2026-09-05), so nothing could. The Jev
Engineering article (0xmovez, 2026-09-18) names the missing piece: a cheap
classifier that judges every tool call for risk before it runs. `typed_ask.py`
asks Jev four absolute yes/no questions about the command in one request, in its
own idiom: does it destroy existing data, does it leave the project or change the
system, does it publish or send, does it read secrets. Obvious secrets (a bearer
token, an `sk-`/`ak_`/`ghp_` key, a `KEY=value`, a `--password`) are redacted
before the command leaves, and the folder's name goes, never its path.

`EVE_COMMAND_RISK`: `off` is the relay as it was; `shadow` (the default)
judges every relayed Bash command and writes the verdict beside the regex class
in the audit log (`source: jev_shadow`) without acting on it; `ask` turns a risky
verdict into your yes/no in the chat with Jev's reason ("[Jev: destroys data,
publishes or sends]") and allows a safe one. A failed verdict (an OpenRouter
error, a timeout, or the asker raising) fails closed: it is asked like a risky
one, labelled `[Jev: unavailable]` with the error kept off the phone, and
logged as `jev_error_asked` when you answer or `jev_error_deferred` when you do
not (2026-10-26). Before that a failed verdict was allowed; with the always-ask
list empty Jev is the only gate, so an outage let every command through. The
cost was measured first: of 647 commands judged in shadow from 2026-10-10 to
2026-10-26, 591 were safe, 50 risky and 6 failed (0.9%), so failing closed adds
about 6 questions to the 50 risky ones. Silence still defers to Claude Code's
own flow, never denies, for a risky and a failed verdict alike: under
`bypassPermissions` or `dontAsk` an unanswered question still lets Claude Code
run the command. The regex always-ask class is asked first and never sent to
Jev.

The ship decision is `tools/command_risk_eval.py --check-default` on
`tests/fixtures/commands/` (74 tuning commands, 59 holdout, both author-written
so the holdout is a tuning set by this project's own rule). 2026-09-21: the
judgement held (0 risky commands judged safe on both sets; 1 of 39 and 2 of 29
harmless ones judged risky) and the clock did not (p50 1.5 s, p90 2.1 s through
OpenRouter's alpha endpoint against a one-second bar), so it ships `shadow`.
Flip it to `ask` if two seconds a command is a price you will pay.

## What you can text

- **Anything you would say at the desk.** Eve answers in a line or three, with
  the earlier turns of this chat as context (the last 24) and, with memory on,
  your profile and today's talk on both channels ([Memory](#memory)).
- **A task for the Mac** — "open the calculator", "play my focus playlist". When
  the request is ambiguous (which account, what to look for once the page is
  open), eve asks one question in the chat first and starts nothing on a
  guess; the planner has the same rule (`computer_agent.py` rule 11: the goal
  as given is the whole task, and a scan with no end in sight becomes a
  question, not more scrolling — live 2026-09-21, "open Amazon" turned into
  2.5 minutes of reading orders). While a task runs, your messages about it
  steer it. It
  runs the same `ComputerAgent` the voice starts, through the same reflex → lane
  → planner tiers ([routing](routing.md)). Eve replies "On it" at once and
  texts the result when there is one. Starting a task costs one model call, not
  two: measured live on 2026-09-21, the second call that only produced "On it!"
  cost 1.9 s, so code says it instead.
- **One progress message per task.** The "On it" message has a red **Stop**
  button under it. Each step the task reports is added to that same message
  (it is edited, at most once a second), so a busy task no longer fills the
  chat. Steps are never cut short. When the message gets near Telegram's
  4096-character limit, the oldest steps scroll off; a step you never saw goes
  as its own message first. A step too long for the message comes whole as its
  own message, and the progress message says "(a long step, sent in full
  below)". A tap on Stop stops that task, as texting `stop` does. It stops the
  task even inside a Codex chat, where the typed word would interrupt Codex
  instead. When the task ends, the message says "Finished" or "Stopped" and
  the button goes. The result comes as a new message, so your phone notifies,
  and it is a reply to the message that asked for the task. If Telegram will
  not edit the message, each step comes as its own message, as Codex steps did
  before (steps from other providers reach the chat only since 2026-09-23). Steps
  still waiting when the work ends come as one message before the result, so
  the last step is never lost. If it will not take the reply link, the result
  comes without it. A restart closes an open progress message ("Stopped." for
  a task, "Closed." for a Codex turn) so no dead Stop button is left behind.
- **A restart mid-answer is said, not swallowed.** If the daemon stops while a
  text turn is still being answered (or is waiting for the one before it),
  eve replies to your message with "I restarted in the middle of answering
  that. Please send it again." Telegram has already delivered that message and
  will not send it again, so without this line the message would be lost with
  no sign (2026-09-24). This is best effort. All of these lines together get at
  most 2 seconds, so a slow Telegram never holds up the restart.
- **The `/` menu.** At startup eve sets its code words as bot commands in
  your own chat only (`setMyCommands`, scoped to your chat): `/apps`, `/stick`, `/spend`,
  `/claude_on`, `/claude_off`, `/new_claude`, `/codex`, `/rundown`, `/watch`,
  `/meet`, `/screenshot`, `/stealth`, `/wake` and `/stop`, plus `/jobs` before `/stop`
  while the chief of staff is on (below). Each works exactly like the
  typed word. If Telegram refuses the menu, the words still work when typed.
- **The menu in plain words** (2026-10-08). Every menu command also works when
  you say it your own way, because the text brain has a tool for each one. This
  is behind the switch `EVE_MENU_TOOLS` (`on` or `off`; unset is
  `menu_tools.SHIPPED`, which is `True` since the v4 blind tool-choice eval passed
  on 2026-10-09). With the switch off, no request carries any of
  it, byte for byte as before. Each
  tool runs the command's own handler, so the result is the same as the typed
  word, with the same pickers. "What did you spend today" is `spend_report`, and
  the brain answers from the same text `/spend` sends (one shared helper,
  `_spend_text`). "Give me my morning brief" is `daily_rundown`: it hands your
  message to a rundown turn of its own, exactly as `/rundown` runs. That turn
  offers only the rundown's read tools, so nothing in your mail or Slack can
  reach a tool that acts. "Hook me into claude" is
  `claude_relay` (on or off). It joins the only running session and asks which
  one when several run. When the session you name is not running, or none is,
  it sends a button to start one and starts nothing itself: a new session opens
  Claude Code with every permission, so it starts only on your tap. "Open codex
  in the eve folder" is `codex_relay`. "Go quiet for a while" and "wake up" are
  `set_stealth`. "Open my eve app" is `open_apps`. "Send me the phone pairing
  link" is `pair_eve_link`; a new link (the old one stops working) is made
  only when you ask for one. The commands that already had tools keep them:
  `start_coding_session`, `screenshot`, `stop_task`, the watch tools, the Meet
  tools, and the chief's, `/america`'s and `/opentable`'s tools. When the
  handler has already sent its message (a button, a picker, the relay's line),
  the turn ends there, with no second model call. An exact code word still goes
  to its code path first, with no model call. The tools are in
  `bridge/src/eve_bridge/menu_tools.py`. Every turn carries them, outside
  the routed toolsets ([toolset router](../toolset-router.md#always-carried-toolsets)).
- **How the brain picks a tool** (2026-10-08, with the same switch). A short
  block of tool-choice lines rides the instructions (`telegram.INTENT_LINES`).
  Each line says when a tool is the right one, in your kind of words, and is
  written only when every tool it names is offered on that turn. A question
  about what eve can do or how something works, a request that starts with
  "don't", and something that already happened are answered in words, with no
  tool. What eve is working on and what waits on you is the chief's
  `jobs_list`. A movie at a theater (what is playing, showtimes) is
  `web_search`; at home it is `firetv_control` opening the app; "I want to watch
  a movie tonight" with neither said gets one short question. A table at a
  restaurant ("make a reservation", "dinner for 4 fri at 7") is
  `opentable_search`, whose description and line now cover any reservation, not
  only one that names OpenTable. When you pick a time, booking it is a
  `start_task` whose goal asks you before the final confirm; nothing books
  silently. Codex by name is `codex_relay`. Concerts and games are
  `events_search`; other current facts are `web_search`. `bridge/tools/intent_eval.py`
  scores these choices ([toolset router](../toolset-router.md#scoring-tool-choice)).
  Widened on 2026-10-08 (v3), by category from the dev failures: the Mac's
  screen is `screenshot` and its files are `list_files` and `send_file`, with
  `start_task` kept for work in an app or on a website (with the switch on, the
  two base lines that sent the screen and files to a Mac task are reworded,
  `telegram.menu_instructions`); forgetting something is `forget_preview`;
  this chat's earlier messages are `recent_messages`; recording a conversation
  is `take_notes`; a show to put on at home is `firetv_control`; your own to-do
  list is `list_todos`; tracking over time is `watch_add`; official government
  sources are `america_ask`; stopping a task is `stop_task`; Codex by name for
  any coding job is `codex_relay`; asking what a running Claude session is doing
  is `claude_relay`; a team's games or an artist's tour are `events_search`.
  Words that name no device ("turn it up") change nothing until a read or one
  question makes the device clear. The rundown tool is for the whole day only.
  Widened again on 2026-10-09 (v4), by category: what a file on the Mac says is
  the new read-only `read_file` (plain text in your home folder, no hidden
  folder, about 12,000 characters at most; anything else is refused with
  `send_file` named instead), after `list_files` when the path is not known; a
  file you want here is `send_file`; telling eve something (your day, a
  friend, something already done) is a reply with no lookup, and
  `memory_search`/`recent_messages` are for when you ask it to recall; a lasting
  preference ("I always want …") is `remember`; capturing a conversation about
  to start is `take_notes`; forgetting is `forget_preview` at once, with no
  search first; a price or a resale price to track is `watch_add` at once;
  sitting in on a meeting is `meet_join`; scheduled and repeating jobs and
  reminders are `jobs_list`, `take_on` and `close_card`; making an app is
  `make_app`; an answer for Claude Code's question is `claude_relay`; stealth
  for a quiet job is `set_stealth`; moving or cancelling an event is the
  calendar's own update or delete tool (a lookup first is fine); CI runs,
  notifications and starring a repo are GitHub's tools, and the workspace is
  Slack, Notion and Linear only.
- **Confirmations for consequential actions** (2026-10-09). Sending, deleting,
  purchasing, publishing, sharing access and comparable consequential actions
  need a concrete yes. Routine reads, written notes, lights, music, local app edits,
  watch setup and ordinary computer tasks proceed without an extra question,
  including after a webpage, message or attachment was read. Deleting (forget, delete an app, remove a watch) always asks. Starting a room recording, joining a meeting or answering a task's question runs on your own request, and asks only when outside text came first, because a page's words could have asked for it. Preparing an
  AgentCard cart is routine; submitting it uses the purchase subsystem's one
  approval for the exact cart. Delegated computer work keeps its final action
  approval rather than asking just to start the task.
  - **Acting and confirming are separate.** `tool_gate.ACTING` still records
    which tools change state. That list is used for provenance tracking;
    `tool_gate.needs_confirmation` decides which native operations need a yes,
    from `tool_gate.POLICY`: every tool eve offers declares exactly one of
    `read`, `routine`, `asks`, `taint_asks` or `executor_owned` (2026-10-26), and
    `bridge/tests/test_tool_gate_coverage.py` fails CI for an offered tool with
    none. An undeclared tool still runs unasked at runtime, so a map that lags
    never puts questions on the phone; the test is what catches it. Declaring
    the policies changed no decision (the test compares every name against the
    old rules).
    The app policies make the corresponding argument-aware decision for
    connected apps. A no or no answer means the consequential call does not run.
  - **What taints.** Every read, and the acting calls that answer with what
    they saw: a new watch's first read of its page, a photo's description, the
    screen, joining a meeting (it answers with the invite's title), and
    `read_file`. `spotify_control` is still an act (routine controls do not ask),
    and since 2026-10-09 (round-4 review) it also taints when it answers with
    the catalog's own words: a play by name, a skip, or "what's playing" names
    a track, an artist, or a public playlist and its owner. A pause, a volume
    change, a transfer, Spotify mode and a plain resume answer in eve's own
    words and taint nothing. The same play typed as a bare music command (done
    by code, no model turn) is kept as outside words. An attached file or
    picture taints its turn from the start. Out
    loud, a Mac task's result, its question to you and `task_status` (the
    task's own words) taint the conversation too (2026-10-09, v3 safety review).
  - **Across turns.** A turn also starts tainted when its prompt already holds
    outside words: a read's line in today's transcript block, a Mac task's
    closing line, or a line eve wrote while tainted (its reply after reading
    mail, the rundown's brief, a task's sentence, a task's or Codex's question to
    you), which the history shows as eve's own words. This is checked on the
    prompt itself (`tool_gate.carried`), so it lasts exactly while those words
    are in the window. **It survives a restart** (2026-10-09): such a line is
    kept marked on disk, as `"outside": true` in the chat log and `"out": 1` in
    the transcript, and every read of the file marks it again
    (`tool_gate.MARKS`), so a restart cannot turn it into eve's own words.
    Also kept as outside words (round-4 review, 2026-10-09): a rundown that
    your vault's todos or Canvas's assignment titles fed, even when no read
    tool ran (`rundown.context_and_outside`; the turn starts tainted by "the
    rundown's sources").
  - **Meet's texts never put an invite's title in the history** (round-4
    review). "I'm in …", the lobby text, the captions text, the notes, a failed
    join and `/meet`'s own "Joining …" show you the meeting's name, which can
    be a calendar invite's title. The history keeps code's own line for each
    instead (`meet.ABOUT_*`, `tell_owner`'s `about`), so the title never comes
    back to the brain as eve's own words.
  - **The question shows the act whole.** A call too long to show whole
    (`tool_gate.SHOW_CHARS`, 1,500 characters) is refused unasked, so you never
    approve words you could not read. **Hidden characters are shown**
    (round-4 review): a zero-width character (U+200B to U+200F, U+2060 to
    U+2064, U+FEFF) or a direction control (U+202A to U+202E, U+2066 to
    U+2069) in a call's arguments appears in the question as `[U+202E]` and so
    on (`tool_gate.visible`, used by the question and by
    `apps_mcp.describe_whole` and `composio_tools.describe_whole`). They are
    shown, not removed, because the call runs with its arguments as written.
  - **Connected apps follow the action policy.** Audited routine operations run
    with or without outside text. Consequential writes ask once, and explicit
    read-only or ask policies still apply. Unknown writes and arbitrary remote
    code remain guarded. The question shows the proposed action; when it cannot
    show the required arguments whole, the operation is refused unasked. A
    pending question is never silently replaced. The history keeps a neutral
    line in place of the question so outside words remain marked.
  - <a id="a-typed-correction-withdraws-the-question"></a>**A typed correction
    withdraws the question** (`EVE_GATE_WITHDRAW=1`; off by default, 2026-10-26).
    Without it, typing "no, send it to Bob instead" while eve waits on a
    send, delete or app question queues your words behind the waiting turn for
    up to `ask_timeout_secs` (180 s); the act is then refused as "No answer,
    so I took that as a no." and a second turn reads your words. With it on,
    the words settle the question at once as a withdrawal
    (`consent.Withdrawn`, which every consent rule reads as a no), the act does
    not run, the question's message ends "Withdrawn: not done. I'm acting on
    what you wrote instead.", and the same turn goes on with the refused call's
    result holding your words (`tool_gate.withdrawn`). No second turn opens,
    and the words reach the brain's history only through that result; the
    transcript keeps them as said. Nothing new can approve: a bare yes, a bare
    no, a button's words and a tap answer exactly as before.
    - **Which questions.** Only the chat turn's own: the check-first gate
      (`TelegramInlet._gate`) and a direct app's or Composio's
      whole-act question (`_direct_tool`, `_app_tool`), marked
      `_Question.withdrawable`. The chief's Go, a share or other outward
      send (`_ask_alone`), a Work session's questions, a Mac task's or Codex's
      question and Claude Code's permission prompts keep their rules.
    - **Which words.** Only a message that would otherwise open a new turn
      (`TelegramInlet._withdraw`, the last step of `_handle`). Text for the
      Claude Code, Codex or Muse relay still goes to the relay and the
      question keeps waiting; so does "eve: …" while a strict prompt waits.
      eve's code words, `/` commands, app commands done by code and quick
      answers run as before. A picture never withdraws. Spoken words keep the
      spoken-answer rules.
    - **Which chat, which moment.** Only the newest live question, only in
      the chat it was asked in, and only one that was already open when the
      message arrived: a message held while "claude on" decides keeps the
      question count it arrived with (`_ARRIVED`), so it never withdraws a
      question opened after it.
    - **Why it is off.** The withdrawn call's result is new text the brain
      reads, so it needs a blind eval (corrections against today's queue)
      before it is on by default (AGENTS.md, rule 6). The tests are
      `pytest tests/test_tool_gate.py -k withdraw`. The bar is
      [docs/evals/gate-withdraw-BAR.md](../evals/gate-withdraw-BAR.md); the harness,
      `bridge/tools/gate_withdraw_eval.py --arm off|on`, drives the real chat
      turn with every acting tool a recorder (its docstring lists the gated
      tools it can drive, also in `tests/fixtures/gate_withdraw_tools.json`).
  - **Out loud.** The voice brain has the same gate
    (`voice_agent.VoiceSession._gate`): a consequential call waits for approval, whether or not outside text was read. **Code says the question, not
    the model** (2026-10-09): `tool_gate.spoken_question` writes it from the
    call's own name and arguments ("Should I start a computer task to: …? Say
    yes to go ahead.") and hands it to the voice to say word for word. Your bare
    spoken "yes" approves that exact call only: the same tool with the same
    arguments, called again after it. Anything else you say drops it. A call too
    long to say briefly (`tool_gate.SAY_CHARS`, 220 characters) is refused
    unasked. When a Mac task asks you something and you answer with a bare yes
    or no, the voice passes that word on (`answer_question`) without asking a
    second time; any other answer is asked about first.
  - **The limit of "word for word", and the check on it** (round-4 review,
    2026-10-09). The voice is a Live speech model: it has no path that speaks a
    fixed text verbatim, and eve has no separate TTS on that path. Code's
    question goes to it as commentary ("Say exactly this…"), and the model
    could say something else. So code checks the voice's own output
    transcript: your yes counts for the held call only after the voice has
    said the act whole, word for word, punctuation and case aside
    (`tool_gate.said_whole`). If it paraphrased, the yes approves nothing and
    the call is asked again. The transcript is the model's account of its own
    audio, so this checks the words, not the sound. A call too long to say
    briefly is still refused unasked.
  - **One yes answers one question** (round-4 review). A spoken yes or no
    answers the question asked last before it: the gate's, or a Mac task's.
    A yes to the gate's question never also answers a waiting task question,
    and a yes to a task question asked after the gate's never approves the held
    call. Each yes or no is used once.
  - **The voice's yes is anyone's.** A spoken yes is whatever the microphone
    hears; the speaker gate that would tell your voice from another's
    (`EVE_VOICE_GATE`, [voice](voice.md#the-voice-gate-only-the-person-who-woke-eve))
    ships off. Someone else in the room can say the yes. This is documented, not
    changed.
  - **The nightly dream reads no outside words** (2026-10-09). The day it
    turns into your profile and records (`transcripts.day_text`) keeps every
    line's place and speaker, but a read's tool line, a task's closing line and
    a line kept as outside words have their words replaced by "(words from
    outside eve, left out)", so a page or an email can never become a fact
    about you. The mem0 index the dream feeds does the same (round-4 review): a
    line kept as outside words goes in as that placeholder
    (`mem0_memory.OwnerMemory.messages`).
  The rule is in `bridge/src/eve_bridge/tool_gate.py`; the chat's question
  is `TelegramInlet._gate`.
- **Concerts, games and shows** (2026-10-08). Text something like "any concerts
  in Seattle this weekend" and the brain uses `events_search`. It asks
  Ticketmaster's Discovery API directly and answers in about a second. Before,
  this went to `web_search`, which can time out. The tool takes a city
  ("Portland, OR" works), a date range in your own days, a kind (music, sports,
  arts & theatre, film, family) and words such as an artist, team or venue. It
  returns up to 8 events. Each event has its venue, local date and time,
  on-sale status, genre, a price range when Ticketmaster lists one (a missing
  range means the price is unknown, not free) and the ticket link. It is offered
  only while `TICKETMASTER_API_KEY` is set. It waits 6 seconds at most and never
  fails the turn: a missing key, a timeout or a refusal comes back as a plain
  reason. Its requests are counted by the watcher's rate limiter, so they share
  one quota with the Ticketmaster watches. "Tell me when tickets drop" is still
  a watch (`watch_add`, kind `ticketmaster`). See [events](../events.md) and
  `bridge/src/eve_bridge/events.py`. It is behind `EVE_MENU_TOOLS` too.
- **`/stick`** (or `stick`, `eve link`, `pair stick`) — pairs the Eve Link
  iPhone app with this Mac: a reply with an **Update Eve Link** button that
  hands the tunnel's address and the link token to the app. `/stick new` (or
  `stick new`) replaces the token; the old one stops working. Answered by code,
  and it works while a relay is on. See [pairing the phone](../action-button.md#pairing-the-phone).
- **`/spend`** (or `spend`, `spending`) — what eve has spent: today,
  yesterday, this month, the top three features today and OpenRouter's own
  figure for today. It is answered by code from the spend ledger, with no model
  call, and it works while a relay is on. The full dashboard is the Mini App's
  Spending view. See [what eve spends](spending.md).
- **`/watch`** (or `watches`, `my watches`) — what eve is watching, one line
  each: the condition, the latest reading and how often it checks. It is
  answered by code, with no model call. `/watch <words>` ("/watch AAPL below
  300") is a watch request for the text brain, which has `watch_add`,
  `watch_list`, `watch_remove`, `watch_pause`, `watch_resume` and `watch_set_end` (a
  watch can be paused, or kept only for a while). An alert arrives as a new message titled
  **Watch** and joins the chat's history, so "stop watching that" in reply
  works. See [watching](watch.md).
- **The chief of staff** (`chief.py`, [the chief](chief.md)), only while
  `EVE_CHIEF` has it on. See the next section.
- **Files a task makes are sent to you.** A texted task on the Mac (a Photo Booth picture, an export, a
  screenshot saved to disk) is told to name the file's full path in its result, and eve sends every
  file the result names that the task made or changed while it ran, as `send_file` does: inside your
  home folder, no hidden folders, under 50 MB, at most five. A file the result only mentions and that is
  older than the task is never sent. Before this (2026-09-25) a task saved a photo, tried to attach it
  through a browser, was blocked, and could only send its path.
- **`/meet`** — the Meet notetaker. `/meet <link>` or `/meet 6pm` (a meeting on
  your calendar) has eve join the Google Meet call from your Chrome, muted
  with the camera off, and text the notes when it ends. Bare `/meet` says
  whether it is in a call; `/meet leave` takes it out. All three are answered by
  code with no model call, relay or not. "join my 3pm" in words is a model turn
  with `meet_join`, `meet_status` and `meet_leave`. The texts are titled
  **Meet**. See [sitting in on Meet calls](meet.md).
- **Lights** — a text that is only a light command ("lights off", "lights
  blue", "dim the lamp to 30%") is done by code with no model call, and the
  reply is one line ("Lights blue."). Anything more ("make it cozy") is a model
  turn with `lights_set`, `lights_match` and `lights_status`. Only with lights set up. See
  [lights](../lights.md).
- **Fire TV** — a text that is only a TV command and names the TV ("turn on
  the tv", "tv volume 30", "mute the tv", "pause the tv", "open youtube on the
  tv", "what's on the tv") is done by code with no model call, and the reply is
  one line ("TV volume 30%."). Bare "pause" or "volume 40" is not, and "play
  music on the tv" stays with Spotify. Anything more is a model turn with
  `firetv_status`, `firetv_control` and `firetv_screenshot`. With no TV paired,
  a TV command gets a one-line "not set up" answer. See [Fire TV](../firetv.md).
- **The camera** — a text that is only a camera command and names the camera
  ("camera left", "turn the webcam right a bit", "zoom the camera in", "center
  the camera", "where is the camera pointing") is done by code with no model
  call, and the reply says where it ended up ("Camera pointing 20° left.").
  Anything more ("point the camera at the door", "follow me around") is a model
  turn with `camera_status` and `camera_control`; to aim at a named thing the
  model takes a photo first. The voice has the same two tools. Only while an
  OBSBOT camera is plugged in. See [OBSBOT](../obsbot.md#asking-eve).
- **"typing…"** shows while eve works on a reply, for the whole turn, not only
  the first 5 seconds. It is sent again every 4 s and stops when the reply goes
  out. `think_hard` keeps it up for up to 5 minutes. It pauses while eve waits
  for your answer to a question.
- **`stop`** (or `cancel`, `/stop`, `/cancel`, or the **Stop** button) — stops
  the running task. This is code, not a model call: it works when the model is
  down or mid-turn. A stop that arrives after the task's agent has already
  returned (while its result is on the way) answers "Nothing is running." and
  the result still arrives; it used to say "Stopped." and eat the result
  (`verification/Eve/TaskStop.lean`).
- **A photo** — "send me a picture of my desk". The robot snaps, the diary keeps
  and captions it as it does for the voice, and the picture arrives in the chat.
- **The screen** — "screenshot", "show me the screen", "what's on the screen".
  A message that is only that is answered by code, no model call, mid-task or
  not (like `stop`). For Codex browser tasks, it sends the last captured view
  from that task's browser tab, labeled with its tab ID. Completed browser tasks
  automatically include this picture. A missing capture is reported; the desktop
  is never substituted. For other tasks, macOS `screencapture` sends the screen
  as a photo and requires Screen Recording for the daemon's Python. Longer
  requests go through the brain's `screenshot` tool. Native tasks that ask to
  *see* something include the desktop picture when they finish.
- **Website access** — `EVE_CODEX_SITE_ACCESS=allow` accepts the runtime's
  ordinary site-access prompts without another Telegram question. Default `ask`.
  Native-app grants, strict safety reviews, uploads, full CDP, authentication
  handoffs, and other action approvals are separate.
- **A file** — "send me the report on my Desktop", "what's the newest thing in
  Downloads". `send_file` sends any regular file under your home folder as a
  Telegram document (50 MB limit); `list_files` lists a folder newest first so
  eve can find the one you mean. Never a hidden path: `~/.ssh`, `~/.config`,
  `~/.aws` and every dotfile are refused after symlinks are followed, and
  nothing outside your home can be named at all. The log gets the size, never
  the name.
- **The robot itself.** A text is a word said to eve: "look left", "look at
  me", "what do you see", "find my mug", "look around", "start taking notes" /
  "stop taking notes", "go explore", "mute", "remember that …" each go straight
  to the same tool the voice has (`move_head`, `look`, `find`, `look_around`,
  `take_notes`, `go_explore`, `set_sound`, `remember`). Only `lesson` (the
  learning workspace, bound to the microphone) stays voice-only. "Start taking
  notes" said on an Eve Link call records the phone instead of the room,
  until the Action Button is pressed ([notes on the go](../action-button.md#notes-on-the-go)).
- **Receipts are reactions.** Where eve used to answer with a one-line
  acknowledgement, it now reacts to your message (owner, 2026-09-23): ✍ for a
  note saved to the second brain, 🏆 for a fact starred with "remember that …",
  👍 for a line typed into Claude, a message that steers a running Codex
  turn, or one that starts a Codex turn (its progress message says "Sent to
  Codex." as well). A turn that only saved or starred makes no second model call.
  A message holds one reaction, so a turn that both starred a fact and saved a
  note gets the 🏆 and says where the note went in words. If the
  reaction fails, the line it stands for is sent instead ("Saved to …",
  "Starred for good.", `Typed.`, "Sent to Codex."). Anything that carries
  information is still a message. The Eve Link app shows the same receipts
  on the owner's bubble, plus 👀 on any photo or file eve reads and ✍ when
  note taking starts ([app-chat.md](../app-chat.md#reactions)); Telegram's own
  set is unchanged.
- **What the robot shows.** While the chat drives a task the robot acts it
  out as it does for the voice: the phase on its face, each progress line and
  the result as a caption on its screen. Stealth hides only the robot: the
  progress message in the chat still updates.
- **`stealth mode`** — a code word (also "play dead", "act asleep"); "wake
  up" / "stealth off" ends it. Asleep, the robot's face goes idle and stays
  there, nothing is shown on its screen, and the head, `find`, `look_around`
  and `go_explore` refuse ("stealth mode: the robot is playing asleep"); the
  camera may still `look`, tasks and files still work. Zero model calls to
  enter or leave it.
- **`codex on` / `codex off`** — `codex on` sends only the names of accessible
  local folders saved in Codex, with debrief folders hidden, and a button for
  each: a tap is the same as typing `codex use <folder>`. Duplicate names show full paths, on the
  buttons too (`~/work/eve`, `~/personal/eve`). No task titles,
  numbers, IDs or old prompts appear. `codex eve` or `codex use eve` starts a
  **fresh chat** in that folder. Each folder selection creates a new conversation.
  Full folder paths also work; unknown or inaccessible folders cannot start a chat.
  The Mac needs to be awake, but no existing task or open app window is required.

  Plain messages (or `codex: message`) continue the new chat, retaining its context.
  During a running turn they steer it, and a 👍 on your message confirms the steer. Each message that starts a turn gets one
  progress message under **Codex** ("Sent to Codex.", then Codex's public steps,
  edited in place, whole, with the same scroll-off rule as a task) with a
  **Stop** button that interrupts that turn, like `stop`. When the turn ends,
  the progress message says "Finished", or "Stopped" if you stopped it. If Codex
  refuses the stop (its turn is still starting), the Stop button comes back. If
  the message never reaches Codex, or the chat disconnects, the progress
  message says so ("Codex did not take it.", "Closed.") and loses its button.
  A step Codex writes just before the turn ends arrives before the answer. The final
  answer is a new message replying to yours. Reasoning and tool output stay out
  of Telegram. Browser tasks
  send the captured image from their own tab using the same validation as Eve's
  computer tasks. `codex status` reports the folder, `stop` interrupts the turn
  while keeping the chat, and `codex off` stops active work and closes the session.
  `eve: ...`, screenshot and stealth commands still address Eve. Switching to
  `claude on` closes the Codex session. Another owner chat cannot control it.

  If startup fails, the next ordinary message goes to Eve. A failed work message
  is never replayed through Eve or automatically retried. After restart, select
  a folder again for a fresh chat. Codex stores the conversation in its own thread
  storage; Eve keeps no extra raw-chat log. Diagnostics record connection stages
  and errors without message text. Earlier raw Telegram texts cannot be recovered
  from those logs.

  Implementation: `codex_chat.py` owns a new persistent thread through the public
  `codex app-server` JSON protocol. It reads saved local project roots from
  `~/.codex/.codex-global-state.json`, deduplicates them and checks filesystem
  access. It does not select or resume any existing desktop task. Routine file
  work is scoped to the selected workspace; approval policy is `on-request`.
  Command approvals show the command and folder in Telegram and require an exact
  affirmative reply. Unsupported permission forms are declined. Native app
  persistence and ordinary website access use the existing Computer Use adapter.
  The model uses Codex's configured default. Desktop-only app tools and the
  built-in browser are unavailable in this standalone runtime; Chrome is the
  default task browser. This does not bypass ChatGPT's native UI restriction.

- **`claude on` / `claude off`** — the Claude Code relay, explicit only. While
  on: the chat shows what the terminal prints in white, as it happens: what
  Claude says (the text blocks of each assistant message, from the transcript
  tailer, under a bold "Claude" title with the repo's folder name beneath it,
  its paragraphs, bullets and code kept), batched every 1.2 s into one message;
  a message over 1500 characters is cut at a paragraph and says the rest is in
  the terminal. The gray
  lines stay on the Mac: thinking is never read, and a tool call and the tail
  of its result are not forwarded (owner, 2026-09-21). "Claude is waiting on
  you" arrives when a session blocks on you, and **what you text goes into
  the session's terminal**: the chat is the terminal. On a call (Mini App,
  stick, Voice PE) a prompt said out loud goes in the same way, and Claude's
  lines that answer it are read on that call; a typed prompt's are not
  (verification/Eve/CallScope.lean). Which prompt a line answers: eve
  keeps each prompt it types, with the call it was said on, in a queue; when
  Claude Code begins a turn, its UserPromptSubmit hook (`turn_begin`) names
  the prompt, and eve takes that prompt off the queue. Every line until the
  next turn begins carries that prompt's call, taken when the line arrives,
  so a typed prompt's answer is never read while a spoken prompt waits behind
  it, and a batch that holds lines of two prompts is sent as two messages.
  A prompt typed at the Mac's keyboard matches nothing in the queue, so its
  turn is read on no call. What is left: the boundary is the next prompt's
  hook, so a line that the transcript tailer delivers after the next turn
  began is read as the next turn's; Claude Code merging a queued prompt into
  the running turn is not seen at all; and with the plugin's
  UserPromptSubmit hook missing, no relay line is read on a call (they are
  all still texted). Plain text is raised into that
  session (`focus_terminal.py`) and typed with Return through System Events.
  A 👍 reaction on your message says it went in; eve sends no "typed" line
  (owner, 2026-09-23), and only if the reaction fails does a short `Typed.`
  arrive instead. Then a "Thinking…" bubble shows in the chat while Claude
  works (`sendMessageDraft` with empty text). It comes back after each of
  Claude's messages and stops when Claude asks you something, waits on you,
  or ends its turn (the Stop hook), or 5 minutes after Claude last said
  anything. Claude's words never go into the bubble: each one is a real
  message, as before. If Telegram refuses the bubble once, eve uses
  "typing…" from then on, which stops at Claude's first words.
  `EVE_TELEGRAM_DRAFTS=0` keeps "typing…" from the start;
  `eve: <text>` talks to eve instead, and eve's code words (`stop`,
  `screenshot`, `stealth mode`, `claude off`) still work. **The relay is
  bypass**: while it is on, the daemon's pretooluse hook allows a tool call
  (and an out-of-repo Read) without asking, the way `bypassPermissions` would,
  because a prompt on the Mac has nobody at it. What Claude needs from you
  still arrives: a question it asks (`AskUserQuestion`, under a "Claude asks"
  title: "Which database? (Postgres / SQLite)"), and a command on your own
  always-ask list (`rm`, `sudo`; `matchers.py`) as a yes/no with Allow and
  Deny buttons, titled "Claude asks to run Bash" with the command as a code
  block. A tap or a short typed yes or no answers it; any other text, even
  one starting with "ok" or "no", still goes to Claude.
  Silence there defers to Claude Code's own flow, never denies. With
  `EVE_TELEGRAM_ASK=1` every call is asked that way. Off by default and
  off again after "claude off": nothing from the terminal leaves the Mac
  until you ask, and the Mac asks as it always did.
- **One shape for every message** (`telegram_format.py`). A message that is
  not eve's own reply opens with a bold title that says whose it is or what
  it is: "Task result" or "Task failed" with the goal in italics under it, "The
  task asks", "Before I do that" (an app action to confirm), "Claude" with the
  repo under it, "Claude asks", "Claude is waiting on you", "Claude asks to
  run Bash". Then a blank line, then the body in short paragraphs: a model's
  markdown becomes Telegram's own bold, italics, `•` bullets, `code` and code
  blocks, never raw stars and hashes; a markdown table (Telegram has none)
  becomes one block per row, the first cell in bold and a `header: value`
  line for each other cell, so a showtimes grid reads on a phone (owner,
  2026-09-24). Everything goes in HTML parse mode with
  `&`, `<` and `>` escaped in content; a message over 4096 characters is split
  on a paragraph boundary with its tags closed and reopened at the cut; a
  piece Telegram refuses to parse is sent again as plain text, so nothing is
  lost to markup. Eve's own replies and one-liners ("Stopped.", "On it.")
  carry no title: in a private chat they are already eve's.
- **No emoji, no dashes.** Every outgoing message is stripped in code
  (`telegram_format.plain`): emoji blocks and their joiners go, an em or en
  dash between words becomes a comma. The prompt says so too; the code makes
  it true.
- **An answer.** When a task needs a yes before something consequential, the
  question arrives in the chat. Your next message is the answer, and only the
  answer. A question without buttons opens the reply box on itself, so the
  answer is clearly a reply to it. No reply in three minutes reads as no. A question whose answers are
  known also has buttons: **Allow** / **Deny** for a yes/no (app actions,
  Chrome access, a Codex command), **Yes** / **No** for "Should I go ahead…",
  and Codex's app-access choices (**Allow once**, **Allow for this task**,
  **Always allow**, **Deny**). A tap is the same answer as typing it. While the
  Claude or Codex relay is on, a question with buttons takes only a tap, a
  plain yes or no, or a button's words typed ("allow for task"). A question
  whose asker goes away first (a stopped task, a Chrome dialog answered at the
  Mac) ends saying "Closed without an answer here.", never that silence was a
  no. A question longer than one Telegram message loses its buttons when
  answered and the outcome comes as a short message below it. Other text
  goes to Claude or Codex, and the question keeps waiting. With no relay on,
  your next message is still the answer, and an image is refused until you
  answer. A typed yes or no in any wording ("ok", "nope") is handed on as the
  buttons' own yes or no, so it does what the tap does. A tap on a picker
  (the new-session tree, say) is never the answer to a different question
  that happens to be waiting. A task's question asked while a Claude
  permission prompt waits is answered first; the prompt then takes a typed
  yes again. This holds for any number of nested questions ending in any
  order: every open question is kept in order, and a typed answer goes to
  the newest one still waiting (`verification/Eve/Questions.lean`). After the answer the question changes to say what was chosen,
  and its buttons go.

One agent drives the mouse at a time. While a spoken conversation is open, or a
task it started is running, a texted task is refused ("the Mac is theirs until
that conversation ends"); while a texted task runs, the voice refuses to start
another. Touching the robot to cancel a task works for a texted task too.

After ten quiet minutes the chat is over: its turns are cleared from RAM and its
transcript gets a close marker. Nothing is handed anywhere, because with memory on
every line was already written down the moment it was typed or sent.

## When a turn runs long

A text turn may call the model up to **10 times with tools**. If the work is
still not done after that, eve makes **one more call with tools off** that
says what it found and did so far, with the specific facts (times, places,
names, links), and what is left. It ends with "keep going", and your reply
carries on from that message. The next turn sees only the text, not the tool
results, which is why the facts go in the message.

Each round's tools are logged by name, and a multi-execute's slugs too
(`telegram: round 3: COMPOSIO_MULTI_EXECUTE_TOOL[GOOGLECALENDAR_CREATE_EVENT]`),
never the arguments.

**The public web is `web_search`, never Composio.** A Composio lookup takes a
tool search plus a multi-execute. eve's own `web_search` takes one round. The
split is enforced three ways:

- **The instructions:** Composio is for your accounts; the public web is
  `web_search`. A job that needs both uses both, for example finding times on
  a course page, then adding them to the calendar.
- **The session:** eve's Composio session is made with an allow-list
  (`composio_tools.TOOLKITS`): Google's apps, Slack, Notion, GitHub, Linear,
  Todoist, Outlook, Dropbox, Zoom, Discord, Trello, Asana and Airtable, plus
  every app you have connected. Nothing on the public web is on it. A deny-list
  did not hold: with Composio's search off, its tool search offered Exa and
  SerpAPI, then Apify, a browser tool, Tavily's MCP variant and Agenty. With
  the allow-list, a tool search for "search the web for a course's office
  hours" offers no web toolkit (checked live 2026-09-30).
  `EVE_COMPOSIO_TOOLKITS=gmail,notion` replaces the list, and `all`
  removes the allow-list: the session is made with Google Drive, GitHub and any
  Gmail or Calendar without a work account disabled instead (2026-10-05). A session stored with a different list (for example after
  you connect a new app) is not resumed; a new one is made at the next start.
- **A guard:** a multi-execute that still names one of those toolkits is
  refused before it reaches Composio, with a reply pointing at `web_search`
  (`composio_tools.WEB_TOOLKITS`). This is what holds under `all`.

Before 2026-09-30 the limit was 6, and running out threw the whole turn away
with "I got tangled up in that one". That happened twice in a row on "find the
office hours for my classes and add them to my calendar". Composio's own
execution log showed every round going to its web search and page fetch. The
last round was looking up the calendar tool, one step short of adding the
events.

## The chief of staff in the chat

The chief of staff ([the chief](chief.md)) takes on jobs with several steps and
reminders you ask for, as cards. It is wired into this door (2026-09-29). `EVE_CHIEF` is
`auto` by default, which is on only once the capture eval has passed its bar.
The eval passed on 2026-09-29 (see [the capture eval](chief.md#the-capture-eval)),
so the chief is on by default. Set `off` to turn it off.

**With the chief off, nothing changes.** The daemon lends the door no chief. Every
request body, reply, tool list and `/` menu is byte for byte what it was before
the chief. A test runs one session on origin/main's `telegram.py` and on this
one, and compares everything each sends. `take_on`, `jobs_list` and `close_card`
are not offered. A stray call to any of them gets "the chief of staff is off on this computer"
and never reaches `think_hard`. `/jobs` and `go c12` are ordinary messages for the
text brain.

**With the chief on:**

- **The tools.** `take_on`, `jobs_list` and `close_card` ride the slot the watcher, Meet, the
  lights and Spotify share. Their lists name only the live step kinds and checks.
- **`take_on` ends the turn.** The chief files the card and composes the
  backbrief in code ("On it: … I intend to … Done when … Up to $1 and 30
  min."). It goes out at once titled **Chief of staff**, with **Change** and
  **Drop** buttons, and there is no second model call. It joins the chat's
  history. If Telegram refuses the buttons, the backbrief goes as a plain message.
- **Your words decide the door, and then they are dropped.** `take_on` gets this
  turn's own message, taken by code, never from the model. It uses the message
  only to mark whether a one-way step is in your own words. The card keeps the
  day and time, never the words.
- **The turn's note.** Each turn's developer note ends with the chief's line
  ("Open jobs: 2 (c12 waiting on your Go: …)"), at most 300 characters. The note
  sits after the history, so the line costs the prompt cache nothing.
- **The code words**, answered by code with no model call, relay or not:

  | Word | What it does |
  |---|---|
  | `/jobs` | The open cards and what each waits on |
  | `/jobs c12` | One card: each step, who ran it and why, each check, the money |
  | `go c12` | Your yes for c12's next one-way step, at the card's current revision. It is refused before the steps that decide the act (the pick) have run |
  | `drop c12` | Drop the card |
  | `keep c12` | Keep it (a proposal becomes active) |
  | `done c12` | Close it on your word; the receipt says "your call" |
  | `raise c12` | Double its budget; a card waiting on the budget goes on |
  | `reopen c12` | Reopen it; a step that failed runs again, and a one-way step needs a new Go |
  | `happened c12` / `didnt c12` | After "I may have done: …": close the act on your word, or wait for a new Go |
  | `tomorrow c12` | Move a reminder to tomorrow at 9:00 |
  | `change c12` | Shows the card and how to change it: drop it and describe the job again. A correction in your own words needs the text brain, whose only card tool is `take_on` |

  A bare "yes" approves nothing that was not asked.
- **The Go.** A one-way step waits for your Go; routine Mac work is not automatically one-way. The question
  names the exact act, the pick it will use and your limits, and has **Yes** and
  **No** buttons. No, a hold word or silence is never a yes. The Go is strict,
  like a permission prompt, relay or not: only a tap, or a reply that is wholly
  a yes or a no, answers it. Any other message goes to the brain as usual, and
  the Go keeps waiting. Before this, with no relay on, "ok, also what's the
  weather tomorrow?" approved the act and never reached the brain (reviewer,
  2026-09-29). On a call it is the same: the exact choice or a plain yes or no.
  No model judges the words, and anything else is asked again. The history gets "(I asked your Go
  for c12.)", never the question, because the pick's name comes from the web.
  A Go is never asked while another question waits for you, because your yes
  would answer the wrong one. The card then waits, and `go c12` works.
- **The Mac.** A step on the Mac uses the same slot, agent, progress message and
  Stop button as a texted task ("On it: c12, step 3."). It never starts while a
  task runs or while someone has the Mac at the desk: the card waits in the
  ledger until the Mac is free. The step's goal is the chief's own, as the Go
  showed it, and no link from the chat is added. The brain cannot steer it:
  `steer_task` on a chief step is refused and names the card, so the owner
  changes the card. Stop still works.
- **Every act runs on Codex.** An approved act asks the daemon for
  `floor="codex"`. `_make_agent` then builds Codex alone, which can stop and
  ask. There is no launch reflex and no other body (the web reader, the Chrome
  lane).
- **The receipt replaces "Task result".** When a step on the Mac ends, its
  closing sentence and the agent's screen text go to the chief. The chief reads
  the evidence and sends the receipt. A step past its ten minutes is stopped
  through the same agent. An error or a stop that the agent reports makes the
  step failed, even when the agent returns a sentence instead of raising. The
  history gets "(Step 3 of c12 ended.)", never the agent's sentence. You never
  saw that sentence, and it may hold web text.
- **The chief's other messages** (a receipt, "Raise to $X?", "I may have done:
  …. Check?", a reminder) go through `tell_owner`, titled **Chief of staff**. The
  buttons match the card as it is at that moment: Reopen; Yes, done and Reopen;
  Done, Tomorrow 9:00 and Drop; Raise and Drop; It happened and It didn't. A tap
  types the button's words, so it takes the same path as the typed word. A Go
  button is never offered on an older message, because the act may have changed
  since.
- **Moments.** A task ending, a wake-word session ending, a Claude relay turn
  ending, and 90 seconds after any message the owner sends (to eve or through
  the relay) count as breakpoints, so a held reminder or a queued Mac step may go
  then. The first message of the
  day gets the chief's brief after the reply.

## What eve knows about itself

**Who it is (2026-10-04).** eve describes itself as your personal sidekick. It lives on your Mac, you
reach it by text and by voice from your iPhone and the speakers at home, and it runs your Mac and your
home's lights, music and TV. The robot body was retired on 2026-10-03. The body's eye and head tools
(`look`, `look_around`, `find`, `move_head`, `go_explore`, `set_sound`) are lent to the brain only
with `EVE_ROBOT=1` in `~/.config/eve-bridge/env`, because lending them made it answer "who are
you" as a desk robot. `remember` and `take_notes` are always lent. Without the body, the instructions leave out
its paragraph too, so they never name a tool the turn does not have (2026-10-05). The
same rule drops `web_search` from the Composio block when `EVE_WEB_SEARCH=off`.

**Photos from the Mac's camera (2026-10-06).** `take_photo` is always lent. Without the robot it takes one still
with the Mac's own camera and sends it here as a photo, in about a second. Before this, "take a photo from my Mac
and send it to me" had no tool, so it went to `start_task`: Codex's Computer Use took about 70 s and the picture
arrived as a file. The instructions say a photo is `take_photo`, never `start_task`. See
[vision.md](vision.md#the-macs-camera) for the camera setting.


On 2026-09-29 the owner texted "what do you use for search", and eve answered
from an old memory record with an engine it had stopped using five days before.
No prompt described eve's own setup, so the brain answered from memory, and
memory goes out of date.

Now `self_context.py` writes a short block about eve's live setup. It has one
line for each area: the model for each door (the Telegram brain, the voice's two
halves, `think_hard`, the Mini App builder), web search, tasks on the Mac, the
watcher's reading ladder, the lights by brand, the memory stores, the doors that
are on, and the git commit the code runs from. Each value comes from the same
`configured()` call the daemon uses to build that door. No value is typed into the
block, and it names only what is live now.

Two lines follow the daemon's own switches (fixed 2026-09-29):

- **Web search** names only the providers that are usable on this machine. It
  reads `search_router.available(env, engine)` under `search_router.mode(env)`:
  with the mode `off`, or `auto` before the router's `SHIPPED` is true, or fewer
  than two usable providers, the line is the one engine ("Perplexity through
  OpenRouter; <model> writes the answer"). Only when searches really are routed
  does it say "Jev picks per query among …", and then it lists the providers that
  have their keys. Jev is shown all three options either way; the code masks the
  ones without a key, so the option list is not what the block reports. The
  router is imported defensively: a tree without it still builds the block.
- **Tasks on the Mac** names the agent `daemon._make_agent` builds: Codex, the
  only desktop executor; "off" with `EVE_COMPUTER_CONTROL=0`. With the web
  reader on, the same line says what a web reading task tries first (TinyFish, or
  the routed search). A test builds the agent through `_make_agent` and checks the
  line against it.

- **Chief of staff** is one line, "Chief of staff: on.", present only while the
  chief is on (`chief.enabled`). With the chief off there is no line, and the
  block is byte for byte what it was. What the chief is doing goes in each
  turn's note, never here, because this block is fixed from boot. With everything
  on the block measured 1044 of 1100 characters (2026-09-29).

- **Canvas** is one line, "Canvas (school courses): on, read only.", present
  only while [Canvas](canvas.md) is set up and the Telegram door is on. The
  school's address is not in it. With everything on, Canvas and the chief
  included, the block measured 1086 of 1100 characters (2026-09-30).

The block goes in three places:

| Door | Where |
|---|---|
| Telegram brain | `telegram.request`: right after `INSTRUCTIONS`, before the profile |
| Voice | `voice_agent.session_config`: right after `INSTRUCTIONS` (the Live front) and after `BACKEND_INSTRUCTIONS` and the memory rules (the backend); also in the think-out-loud update |
| `think_hard` | `think.request`: right after `INSTRUCTIONS`, before the owner's background and the clock |

It ends with one rule, scoped to what the block is: the source of truth for the
models, services and providers eve runs on, above memory and past chats. For
those, when the block does not name one, eve says it does not know instead of
guessing. What eve can do is its tool list, not this block, so a question about
the calendar, Spotify or the camera is answered from the tools. (The first rule
covered "tools" too, and would have made the brain say it did not know about its
own tools.)

Each door's config carries the block (`about`). Its `configured()` builds the
block once at boot, from the same environment and lights file as the daemon's live
objects. The block does not change from turn to turn, so it stays in the stable
prefix that the prompt cache reuses. The clock stays in the turn note. A config
built by hand, as in the tests, has an empty block, and the prompt stays the same
as before. With memory off, the Telegram instructions are `INSTRUCTIONS` plus the
block and nothing after it (`tests/test_daemon_transcripts.py`).

Size: under 1,100 characters (`self_context.BUDGET`; 1,000 before the tasks line).
Measured on 2026-09-29, the block was 859 characters on the owner's setup and
1,022 characters with every component on (routed search, voice, all four light
brands, and the alternative desktop executor removed on 2026-09-30). `tests/test_self_context.py` covers the budget, checks that each
door carries the block, and greps every prompt the three doors send (tools
included) for the retired engine name.

Memory was fixed at the same time, so no retired engine name reaches a prompt
through the profile (it rides every Telegram prompt with its record index):

- The stale preference record is now `preference-web-search-routing`. Its line is
  the owner's preference (routing with Jev across TinyFish, Perplexity via
  OpenRouter and Firecrawl), not a claim about what is live; it points to the
  self-context block for that. Its line in `profile.md` says the same.
- The 2026-09-21 conversation record was renamed to
  `conversation-websearch-and-spotify-control-2026-09-21`, and the aliases that
  named the engine were dropped. Its body is history and was kept as it was. The
  profile's record index was regenerated with `records._reindex_profile`, not by
  hand.
- The copies from before each change are in
  `~/.config/eve-bridge/memory/backup-2026-09-29-self-context/`.

Checked on 2026-09-29 with the real profile and the owner's environment: the
Telegram, think and voice requests had 0 hits for the retired name. The positive
controls found it (the block on a setup that runs that engine, and the profile
from before the rename). Still outside these fixes: today's transcript quotes
eve's own wrong answers (history, gone from the prompt when the day ends), and
the mem0 index still holds one memory from 2026-09-21 that names the engine.

## The chat's history (the chat log)

On 2026-10-08 a bot asked "Before that" answered with the newest message again. It had nothing earlier to read:
the history lived in RAM, ten quiet minutes or a restart emptied it, memory is off on a fresh install, and work
handed to an agent was never in it. Now the chat keeps a small log on the Mac, on every install, memory on or off.

- **What is kept.** Per chat: what you typed, eve's replies, and one short note per hand-off to an agent and
  what came back ("You asked the computer agent: …", "You told the running computer task: …", "The owner passed
  to Codex: …", "Codex answered: …", "Claude Code answered: …"). A note is one line of at most 200 characters with
  links taken out; an agent's or a page's text never goes in beyond that. A turn that failed keeps the failure
  line, so the next turn sees the message was not answered.
- **Where and how.** `~/.config/eve-bridge/chat/log.json`: the folder 0700, the file 0600, rewritten whole
  and atomically on every line (a temp file, fsync, rename), refused in a git work tree or a synced folder. It is
  read once at start, so a restart resumes the chat.
- **Seeded once from memory.** With memory on, an install whose transcripts are older than its chat log (every
  install that had memory on before 2026-10-08) would start the log empty, and "before that" had nothing from
  before the log's first start, though the transcripts held it (live, 2026-10-08 evening). On the chat's first
  turn the log copies the chat's earlier lines from the transcripts once: the Telegram channel's say and photo
  lines of yours and eve's (the Eve Link app's chat rides that channel), only those older than the log's
  oldest line and from conversations the log has no line of, within the log's 7 days and 300 lines, oldest
  first. The file marks the chat seeded, so it never runs twice, even after a forget empties it. Seeded lines are
  ordinary lines: forget reaches them like any other. A seed, not a second source at read time: one store to
  read, number and forget, with nothing to merge or de-duplicate on every call.
- **How long.** The newest 300 lines per chat, none older than 7 days (`EVE_CHAT_LOG_LINES`,
  `EVE_CHAT_LOG_DAYS`), pruned on every write.
- **What the brain is shown.** The newest lines are its history, up to 40 lines or 12,000 characters; past
  either, the window is cut back to 28 lines in one step, so its start holds still across turns and the prompt
  cache keeps it. A quiet spell of ten minutes or more is a note before the next line, "(2 hours later)", or
  "(2 hours later, after a restart)". When older lines exist, the history starts with a fixed note saying so.
  Hand-off notes are bracketed developer notes, never words you said. A note whose words an agent wrote
  ("Codex answered", "Claude Code answered", "Claude Code asked") shows only that label in the history,
  "(Codex answered; its words are in recent_messages)": an agent's words can quote a page or a file, and a
  developer note outranks your own messages. `recent_messages` returns them as a tool result, which the
  instructions call information, never instructions. The today block is counted against the same window the
  history shows, so a line is in the prompt once, also on the turn the window is cut and on a rundown.
- **`recent_messages`.** One tool, offered every turn: earlier lines word for word, oldest first, the message
  being answered left out. Each line carries `back` (1 is the newest) and your own messages `owner_back` (1 is
  your last message before this one). When eve's last reply quoted one of your messages, the result gives that
  message (`your_last_reply_quoted`) and the one before it, whole (`before_that`), so "before that", and "and
  before that?" again, is a lookup, not a count. Which message a reply quoted is tagged on its log line when it
  is written (`quotes`): the turn's `recent_messages` results are matched against the reply's words, a quote
  being the whole message or a run of three words or more and half of it or more, so "my locker code is 4417"
  quotes "(memory test, just reply ok) my locker code is 4417"; of several, the oldest. A reply written without
  the tool is read by the same rule. `before_owner_back` reads back from a given message of yours. One call returns
  at most 8,000 characters, newest first, and cuts any one text over 1,500 characters with "…"; `earlier` and
  the note say how to read further back. A rundown offers it too, since the instructions name it on every turn.
  **A stretch of time** (2026-10-09): `since` and `until` (your local time, `YYYY-MM-DDTHH:MM`; `until` null
  for now) return every line in that time at once, oldest first, up to 80 lines and 16,000 characters; past
  that, the note gives the count left and the `since` to read on from. The tool's description tells the brain
  to use it for "what happened last night", "this morning" or "yesterday", and that last night runs from 18:00
  to 04:00. Before it, "what happened last night" read the newest 20 lines and summed the night up without
  what came earlier (live: a party left out, found only when you asked "before that").
- **The conversation going on now.** "Open that link" and the router's follow-up context read only the lines
  after the newest quiet spell (ten minutes) or restart, never a link from hours or days ago.
- **When it is not there.** The instructions say what the brain knows of earlier messages, as a fixed set (the
  history, today's talk and `memory_search` with memory on, and `recent_messages`), and that past it, it says
  plainly that it does not have it and never repeats or invents a message.
- **Forget.** With memory on, `forget_preview` / `forget_apply` and `eve-bridge memory forget` count and
  remove matching lines from the chat log too (the `chat_log` layer, [memory.md](memory.md#forget)); a daemon
  that finds the file rewritten by the CLI reads it again before its next write. With memory off there is no
  forget tool: turn the log off, or delete the file while the daemon is stopped.
- **Off.** `EVE_CHAT_LOG=0`: no file is read or written, no `recent_messages`, no notes, and the history is
  what it was before: RAM only, cleared after ten quiet minutes and by a restart. The one line kept is the
  instruction that earlier messages are not kept, so the brain says so rather than inventing one.

Measured with the live brain (`bridge/tools/chat_memory_eval.py`, twelve scripted conversations in
`bridge/tests/fixtures/chat_memory_eval.json`, each ending in a recall question): main's chat with memory off
scored 3/12 and 2/12; with the chat log, the final code scored 12/12, 12/12 and 11/12 over three runs (rescored
with the final scorer; 11/12 each as first scored). The case it still misses on some runs is "the message I sent
before my last one", which the brain sometimes reads as counting the question itself.

The same evening the owner's own daemon (memory on, the log brand new) answered "before that", right after quoting
his last message, with that message again. Four cases reproduce it (`live-*`: the earlier talk only in the
transcripts, already in the log across a restart, two quiet hours before the locker line, and "and before that?"
twice); they run with a real memory over temp transcripts and ask each question as a live turn. The code before
the fix scored 2 of the 4 (the seeded case abstained, the twice case repeated the quoted message, as live). After:
the four passed 5/5 each over five full runs, and the twelve scored 12, 12, 11, 12 and 12 (the miss: the case
above, which the instruction "the message you are answering is not counted" took from 1/6 to 6/8 in spot runs).

## Memory

With `EVE_MEMORY=1` the text brain shares one memory with the voice. The full
design is in [memory.md](memory.md); what matters for the chat:

- **Every line is in the day's transcript as it happens**
  (`~/.config/eve-bridge/memory/transcripts/<day>.jsonl`): what you typed,
  eve's replies, commands, relayed Claude lines, image turns and the tool results
  eve answered from. A restart or a failed model call loses nothing, and a voice
  conversation a minute later sees this chat.
- **Every turn's prompt carries** the profile (`records/profile.md`, re-read each
  turn, up to 6,000 characters, with the stars the nightly dream has not absorbed
  yet on top) and today's lines on both channels (up to 32,000 characters, minus
  what this chat's history already shows). The opening brief rides in the turn note.
- **The chat history holds only what was said to eve.** Words you type to
  Claude Code or Codex through the relay, your answers to Claude's permission
  prompts, the prompts themselves, and code words (`claude off`, `stealth`,
  `screenshot`, ...) go into the transcript as `relay` or `command` lines. They
  never enter the history the text brain is shown, and the today block leaves
  those kinds out too. Claude's replies never entered the history, so the brain
  saw only one side of those exchanges. On 2026-09-24 it made a "plan" from eight
  lines typed to Claude Code.
- **Four memory tools:** `memory_search` (record lines, mem0 memories found by
  meaning, and the words said, each dated), `memory_read` (a record by id, or a day
  or a stretch of one), and the two-step forget, `forget_preview` then
  `forget_apply`, which runs only after you confirm in a new message.
- **"remember that …"** goes through the `remember` tool into
  `records/starred.md`, and counts from your next message. A 🏆 on your message is
  the confirmation.
- **The brain never writes what it knows.** The records and the mem0 index are
  rewritten once a night by the dream, from the transcript.

With memory off there is no transcript, no profile and no memory tool. The chat log
([above](#the-chats-history-the-chat-log)) is separate and kept either way.

## The rules, and why they are code

This door reaches a Mac with full computer control. None of these is a prompt
instruction; each is a branch in `telegram.accept` or the inlet, with a test.

| Rule | Why |
|---|---|
| Only an owner id, in a private chat, reaches a model. Anyone else gets no reply at all. | A reply tells a stranger the bot is alive. A group the owner is in carries other people's words. |
| A forwarded message never reaches a model (you get one fixed line back). | It is the easiest way to put someone else's instructions in front of an agent that can click. |
| A message older than two minutes when it arrives is dropped. | Telegram holds undelivered messages for a day. A task texted while the daemon was down must not run when it comes back. |
| Only your next message can answer a task's question. | "Only the human approves" ([routing](routing.md)) has to hold over chat too. |
| A button tap acts only when it is from an owner id, in that owner's private chat, on a button this run of the daemon made and still needs. Every tap is answered; a stranger's tap is not. The owner's own tap outside their private chat is answered empty and does nothing. | A button's data is only a short key, and what it means stays on the Mac. A button from before a restart, or one already used, says "This button has expired." and does nothing. |
| Edited messages, channel posts, other bots, stickers and voice notes reach no model. | Only new text from the owner is a request. |
| A file leaves only from your home folder, never from a hidden path, symlinks followed first. | A chat that can reach a Mac must never be a way to read its secrets. |
| What you wrote is never logged; neither is the token. | The log gets counts, seconds and numeric ids. The token is part of every Bot API URL, so HTTP errors are rewritten before they are raised (`BotApiError` never carries a URL) and every log record in the process is checked for the token as it is made (`hide_token`) — httpx logs each request line at INFO. |
| `401`, `404` or `409` from Telegram stops the door with one log line. | A wrong token or a second poller will not fix itself; spinning on it helps nobody. Anything else backs off (1 s doubling to 60 s) and resumes. |

A stranger's first message produces one log line — `telegram: dropped a message
(stranger) from user id N` — once per id, which is also a second way to find
your own number.

## What leaves the Mac

Your messages and eve's replies pass through Telegram's servers (bot chats are
not end-to-end encrypted). Each turn is one or more Responses API calls with
`store=False`: the turn is stateless, the model's own tool calls and encrypted
reasoning are sent back each round instead of a `previous_response_id`, and
Responses API conversation state is not stored. This is not a guarantee of zero
data retention under the provider's other policies. A texted task sends screenshots to
the planner exactly as a spoken one does. The apps Mini App has its own path: build requests go
to Anthropic, and traffic passes through a Cloudflare tunnel
([the Mini App](miniapp.md#what-leaves-the-mac)).

## Not done

- The chief of staff on the voice door: the voice has only the self-context
  line, and keeps its own `start_task`.

- No evaluation set for the text brain exists, so it ships off (`GATES.md`).
- Voice notes, animations and non-image incoming documents are not supported.
- One owner conversation in RAM. Several owner ids share the RAM history when the chat log is off; with it on,
  each chat's log is its own.
