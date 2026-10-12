# Eve → Codex Computer Use

Verified on 2026-09-22 with desktop runtime `codex-cli 0.155.0-alpha.16`,
ChatGPT desktop/unified-computer-use `26.917.51856`. Discovery also succeeded
with Homebrew `codex-cli 0.144.1`; native execution was verified with the bundled
runtime. This is a local capability result, not a claim that every CLI install
ships Computer Use.

## What is implemented

Eve's existing callable `start_task(goal)` now uses `CodexComputerAgent`, via
`Daemon._make_agent`, for both voice and Telegram. Each task launches its own
`codex app-server --listen stdio://`, initializes the documented protocol,
creates an ephemeral thread, and paginates `mcpServerStatus/list` to require
`cua_repl` with `js` before `turn/start`. It inherits Codex's configured model.
The adapter requests a read-only filesystem sandbox and on-request approvals.
Computer Use remains subject to its separate app and OS permissions.

- Completed commentary events relay as progress; final agent messages return to
  Eve. Native UI observation evidence is retained in the adapter's task state;
  a final answer without it explicitly says Eve has no native UI verification.
  Evidence presence alone is not a claim that the requested outcome was achieved.
- Non-automatic empty Computer Use permission forms go through the existing voice/text question
  path. `yes` allows once; `allow for task` requests session persistence;
  `always allow` requests persistence for that app in future tasks. The latter
  choices appear only on recognized native app-access requests and only when
  offered by Codex's `persist` metadata. Codex owns storage and revocation; Eve
  never writes the approvals file or maintains an auto-approval cache.
- Other Codex user questions relay through the same path. Unhandled permission
  forms and operations are declined with a user-facing explanation.
- `EVE_CODEX_ROUTINE_ACCESS=allow` (default, owner request 2026-09-25)
  accepts recognized native app-access grants, HTTP(S) site access and downloads
  without another voice/text question. It also accepts uploads to the exact
  `https://web.telegram.org` origin. `ask` restores access questions;
  `EVE_CODEX_SITE_ACCESS` overrides the site preference independently.
  Decisions use structured runtime metadata, never words in the prompt. No
  global grants or fabricated automated reviews are written. Native app risk is
  a capability warning; app access does not authorize consequential actions.
  Both computer tasks and folder chats receive the owner's instruction to ask
  before purchases, deletions and sends/publication/uploads outside Telegram.
  This semantic action boundary is an agent instruction, not a classifier of
  arbitrary clicks. Action-specific, unknown, raw CDP, history, authentication,
  audio and strict security requests remain on the existing question path.
  Runtime managed denials remain enforced. Phone calls use Telegram's dispatch
  path and therefore receive the same policy. Restart Eve and start a fresh
  folder chat to load code/instructions; prewarmed tasks refresh on restart.
- Browser tasks capture their selected tab in the final Computer Use call. Eve
  accepts a picture only when the capture's tab ID matches the browser state in
  that same successful tool result and the image bytes validate. Later Computer
  Use actions invalidate the picture. Telegram sends this picture automatically
  after a browser task, including requests that did not explicitly say screenshot.
  Follow-up screenshot requests use the last captured task view and label it as
  such. If unavailable, Eve reports that instead of capturing the desktop.
  Browser state also counts as UI evidence; it is not native-app state.
- `steer_task` uses `turn/steer`; `stop_task` cancels pending questions and sends
  `turn/interrupt`, with process termination as bounded cleanup. No prompt retry
  is attempted after a disconnect or ambiguous error.
- No shell UI driver, private desktop IPC, copied session credentials, newly minted API key,
  or alternate computer-use implementation is involved.

`EVE_CODEX_BIN` optionally overrides the executable. The default prefers
`/Applications/ChatGPT.app/Contents/Resources/codex`, then `codex` on PATH.
The adapter passes ordinary OS environment variables and optional `CODEX_HOME`;
Eve's keys and desktop session/pipe variables are not forwarded. Codex uses
its own ordinary local sign-in/configuration. A task has a ten-minute wall limit.

## Saved logins, cards and 2FA codes: `eve_vault`

When the vault holds an item, each task's thread is started with eve's `eve_vault`
MCP server (`config.mcp_servers` on `thread/start`) and a paragraph telling Codex to
call `vault_fill` instead of typing a password, card number or code. `vault_fill`
finds the field in the page through the Accessibility API and sets its value: Codex
never sees it. Codex's approval forms for `vault_list` and `vault_fill` (Codex 0.159
names the tool only in the form's message) are accepted without asking the owner;
nothing else is. A prewarmed thread started before the vault had items, or after it
lost its last one, is replaced on the same app-server when the task starts. Verified
end to end on 2026-10-12 (`bridge/tools/vault_codex_e2e.py`): a real task signed in
to a local page, whose server received exactly the saved password. See
[the vault](../vault.md).

## Separate API-key billing

`EVE_CODEX_AUTH` accepts `plan` (the default) or `api`. Plan mode preserves
the original child environment and uses your existing Codex sign-in, including an
explicit `CODEX_HOME`. API mode selects Eve's own home for every new app-server:
computer tasks, folder chats, and the prewarmed agent. Existing processes retain
the identity they started with; changing the setting takes effect on a future
daemon start.

1. Put an `OPENAI_API_KEY` in `~/.config/eve-bridge/env`. This mode is optional:
   it is the only use of that key apart from the desk voice. Every other model call in
   eve goes through `OPENROUTER_API_KEY` ([one key: OpenRouter](../openrouter.md)).
2. Run `eve-bridge codex-home` using the desktop Codex binary, or the binary
   selected by `EVE_CODEX_BIN`.
3. After verifying Computer Use works with that API identity, set
   `EVE_CODEX_AUTH=api` in the env file for your next daemon start. Setup
   itself does not enable the setting or restart the daemon.

The command creates `~/.config/eve-bridge/codex-home` with mode `0700`.
It copies `config.toml`, `computer-use/`, `browser/`, `plugins/`, the generated
`.tmp/bundled-marketplaces/` catalog, and `chrome-native-hosts-v2.json` when present.
These paths were inspected in the installed desktop distribution: plugin
manifests, MCP launch configuration and helper binaries live under `plugins/`,
while `config.toml` references the generated bundled marketplace. Embedded paths
in the copied JSON/TOML configuration are redirected to Eve's home. Runtime
files are independent copies; refresh does not leave links back to personal files.
External-link copies are refused.

It never copies `auth.json`, MCP OAuth credentials, session databases, account
caches, or desktop IPC. The copied config uses file credential storage and API
authentication, so a personal keychain preference cannot redirect login. The key
is passed through stdin to `codex login --with-api-key`, never through argv,
logs, or the repository. The resulting private `auth.json` has mode `0600`.
Your personal `~/.codex/auth.json` and config are never changed. Running setup
again refreshes runtime configuration while preserving the private API sign-in;
it does not require the key again or silently rotate credentials.

If API mode is requested but the private home has no valid API-key sign-in,
Eve logs one warning and uses the original plan environment. Invalid setting
values also fall back with a warning. This fallback is for missing local setup;
an API service rejection during a task is reported without retrying on the plan.
If the Computer Use plugin is absent, Eve refuses the task as before and uses
no alternate computer-use implementation. Keep the setting on `plan` if the live
compatibility check fails.

Plan tasks remain unpriced `chatgpt` / `codex` entries. API tasks are recorded as
`openai` / `codex`, with the note `API key, billed per token`. When the app-server
reports `thread/tokenUsage/updated`, the adapter uses per-turn differences in
cumulative input, cached-input and output tokens and the actual selected model's
rates from the spend price table. Reasoning tokens are already included in output.
Missing or incomplete usage, mixed-model turns, and unknown model rates leave
the entry unpriced. An interrupt acknowledgement alone is not final usage. This
is an estimate of model token charges, not a reconciliation of all hosted-tool fees.

Official [Codex authentication documentation](https://learn.chatgpt.com/docs/auth)
describes API-key login, `CODEX_HOME` file credentials, and plugin limitations.
The installed runtime's MCP inventory and a real task remain the compatibility
check for this feature.

### Live compatibility result (2026-09-28)

Verified with `codex-cli 0.155.0-alpha.16.4` after the full test suite passed.
The real `codex-home` setup completed, and `account/read` returned account type
`apiKey` from the private home. The relevant `mcpServerStatus/list` entry was:

```json
{"name":"cua_repl","tools":["js","js_reset","turn_ended"],"authStatus":"unsupported"}
```

Here `unsupported` is the MCP server's authentication-status field; the tools
were loaded and usable. A read-only task using model `gpt-6-astra` called the
installed `cua_repl` and returned fresh Calculator accessibility state:
`Window: "Calculator", App: Calculator` and `Edit field, Value: 4`. Its final
answer was `Title: Calculator` / `Display: 4`; it entered no calculation and
required no permission response. Computer Use therefore worked with this
API-key login on this installed runtime.

The task produced an `openai` / `codex` spend row with 73,306 input tokens,
46,595 cached input tokens and 154 output tokens, estimated at $0.321405 by the
existing price table. The personal auth file's SHA-256 matched the pre-setup
checksum, and the original main worktree's diff and status were unchanged.
The feature remained configured as `plan`; no daemon restart or push was made.

## Investigation evidence

Architecture: `telegram.py` and `voice_agent.py` already had task/steer/stop,
progress, question, and completion interfaces. The daemon previously supplied
`ComputerAgent`, a separate Responses/PyAutoGUI worker. That worker remains in
the repository but is not the default voice/text task backend anymore.

Telegram folder chats now use `codex_chat.py`, a persistent session through the
public app-server. `codex on` lists accessible saved folder names only;
`codex eve` creates a new chat in that folder. It does not attach an existing
Mac task or depend on private desktop IPC. Subsequent messages retain context;
`stop` interrupts a turn and `codex off` closes the session. Failed startup
returns to Eve. The earlier `codex_relay.py`, which attached to existing Mac
tasks, was removed on 2026-09-23 once nothing imported it; it is in git history
before that date.

Installed metadata inspected:

- `~/.codex/plugins/cache/openai-bundled/unified-computer-use/26.917.51856/.mcp.json`
  launches the packaged `@oai/cua-repl` and exposes `js`, `js_reset`, `turn_ended`.
- Its plugin manifest describes an app-managed runtime with stop/interrupt hooks.
- The installed `@oai/cua-repl/README.md` describes desktop configuration of the
  package and inheritance of approval/permission settings.
- `codex-app-tools` is a different app-managed bridge requiring a host-provided
  pipe. Its tools are unavailable in the standalone probe; they were not copied
  into Eve or treated as a public task-creation endpoint.

The public app-server inventory **did** expose `cua_repl.js` in both installed
runtimes. A direct call outside a model turn enumerated native apps, while browser
inventory reported missing turn metadata. This was not treated as a successful
browser test. A real ephemeral turn then used `cua.getApp("Calculator")` and
emitted the documented `mcpServer/elicitation/request`. Declining prevented app
access. After the owner explicitly approved Calculator for the test, the real
turn computed 2+2 and returned fresh AX state with result 4.

The same test was then run through Eve's actual `start_task` dispatcher and
daemon factory. [native-smoke.json](native-smoke.json) records fresh native UI
state, progress relay, and permission relay. Its Telegram transport was an
in-memory sink; no Telegram message was sent. The actual Codex runtime and native
Calculator app were used. Browser execution, microphone/robot voice hardware,
and a real driver repair were not exercised by that smoke test.

## Limits and fallback behavior

Computer Use must already be installed/enabled and the OS grants must exist.
In the 2026-09-23 live browser check, the standalone session could use Chrome's
browser API but reported `Browser is not available: iab` for the built-in browser.
Eve therefore uses Chrome when the owner has not specified a browser. Explicit
built-in-browser requests report its unavailability rather than substitute one.
Missing tools, denied requests, unsupported forms, and disconnects are reported;
Eve does not substitute its old worker. Some app-server builds or desktop
plugin updates may behave differently. Capability discovery runs on every task.
The adapter does not create/resume a desktop-owned task, adopt its transcript,
or impersonate the desktop host.

For “fix the blocked Yeti driver,” Codex can inspect the UI and guide the work,
but cannot approve OS security/privacy prompts or authenticate as administrator.
Those steps must be done by the owner. If a task needs an unsupported form or
desktop-only service, the smallest supported alternative is to continue in the
Codex desktop UI (or its official Remote client), retaining app controls there.

## Official sources consulted

- [Codex App Server](https://learn.chatgpt.com/docs/app-server): stdio initialization,
  thread/turn lifecycle, MCP inventory, elicitation, streaming, interruption.
- [Codex SDK](https://learn.chatgpt.com/docs/codex-sdk): local Codex agents and
  app-server clients; no assumption of automatic desktop capability inheritance.
- [Computer Use](https://learn.chatgpt.com/docs/computer-use): desktop plugin setup,
  app access, OS permissions, and prohibited security/admin prompt automation.
- [Codex Remote](https://learn.chatgpt.com/docs/remote): official phone-based task
  progress, approvals, and follow-up instructions.

The public protocol plus the measured installed plugin behavior establishes this
path on this Mac. The docs alone do not promise Computer Use in arbitrary SDK or
app-server deployments.

## Additional Telegram verification

A live `rundown` run used Gmail and Google Calendar through the configured
Composio session and scanned the configured Obsidian vault. The returned brief
reported partial email coverage and Slack disconnected before authorization.
After the owner connected Slack, a second live run verified all three app
connections and successfully executed SLACK_SEARCH_MESSAGES along with Gmail
and Calendar reads (Composio logs log_5RPX6e24EzX2 and log_nPDO2B0SeUYY).
It reported no direct mentions today, unavailable unread state, and partial
broader Slack results. Both tests used an in-memory Telegram sink and did not
send mail or chat messages.

Image intake tests exercise the Bot API download boundary, actual image decoding,
private temporary files, Responses image payloads, and routing to both existing
relays. Relay endpoints are faked in these tests; no attachment was injected
into an active user session. The existing experimental desktop task follower
still has its private-protocol limitation; the new Computer Use adapter does
not depend on that follower. Image routing adds no endpoint or permission grant.

Final regression run: **1,826 passed, 1 skipped** (73.69 seconds), excluding
`test_desktop_live.py`; those legacy desktop-control tests were not run. The
suite required local socket access for fake servers; the sandboxed attempt
failed on restricted sockets/native facilities. All eight scoped acceptance
gates passed, including re-execution of the four runnable gates. Changed-file
lint and `git diff --check` passed.

Eve's idle launch agent was restarted on 2026-09-22 at 19:46 PDT. Its status
socket returned healthy, startup reported Codex Computer Use ready, and Telegram
reported listening for the configured owner. Relay selection is in memory;
select `claude on` or `codex <folder>` again after restart.

## Remembered app permissions

The original adapter exposed only one-time decisions. It now forwards the
installed Computer Use native approval choices through the public app-server
response: `{"action":"accept","content":{},"_meta":{"persist":"always"}}`,
or `persist: "session"` for the current task. This is the same response used by
the installed desktop app's Always allow button. The generated app-server schema
explicitly accepts response `_meta`. A live metadata-only probe observed
Calculator (`com.apple.calculator`) offering `persist: ["session", "always"]`;
that probe declined the request and changed no grants.

Eligibility requires the installed native policy wrapper's exact connector,
approval kind, app-only parameters and a known native operation name. Audio,
URLs, unknown operation types, structured forms and action-specific requests
cannot be remembered as app access. If managed policy removes `always`, Eve
neither offers nor sends it, even if the owner types that phrase. Risk warning
subtitles are shown with the question. `yes` retains one-time semantics.

Codex checks app policy before native permission requests. Saved app access does
not authorize OS security/privacy prompts, administrator authentication, or
consequential actions that require separate confirmation. Review and revoke
saved grants in **Settings → Computer Use → Always-allowed apps**. See the
[official permission behavior](https://learn.chatgpt.com/docs/computer-use) and
[managed persistence controls](https://learn.chatgpt.com/docs/config-file/config-reference).

Live persistence was verified after the owner explicitly approved Always allow
for Calculator: the first Eve adapter session received one native permission
request and forwarded `_meta.persist: "always"`; a separate fresh app-server
session received zero requests. Both observed Calculator's displayed value `4`
from fresh native UI. See [permission-smoke.json](permission-smoke.json).

## In front of Codex: app launches and a warm agent

A bare "open <App>" never reaches Codex. `app_reflex.py` opens it with
`open -a`: the rules take it first, and Jev handles wording they don't know.
The Codex agent is built only for everything else. That agent is usually
already started: `codex_warm.py` keeps one prewarmed, meaning its app-server,
thread and `cua_repl` check are done, so the handoff starts the turn straight
away. The measurements and the model comparison are in
[routing.md](../stackchan/routing.md#opening-an-app-in-front-of-codex).
