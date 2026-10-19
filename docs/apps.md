# Connected accounts: direct, and Composio for the rest

This page is about **connected accounts**: services eve signs in to on your behalf
(Google, GitHub, Maps, and Composio for the rest). eve's own **apps** (lights, music,
a camera, a TV, a car: modules in this repository that drive a device) are a different
thing; to build one, see [adding an app](adding-an-app.md).

eve reaches your everyday accounts **directly**, with no broker in the middle:

| App | How eve reaches it | What it may do |
|---|---|---|
| Gmail | Google's Gmail API | Read: search, read threads, messages and attachments. Send, after you say yes to each email, shown whole. Never labels or deletes. |
| Google Calendar | Google's Calendar API | Read and find free time. Create private events and edit without guest delivery. Invites, replies, deletion and guest notifications ask first. |
| Google Drive | Google's Drive API | Read anything. Create a new private file without asking. Sharing, deletion and changes with an unknown destination ask first. |
| Google Contacts | Google's People API | Read only: find a person's email address or phone number, among your saved contacts and the people you have emailed. |
| GitHub | GitHub's MCP server | Read repos, issues, pull requests and CI runs. Create an explicitly private personal repository without asking. Publishing, issues, comments, PRs and other changes ask first. |
| Google Maps | Google's Maps MCP server (Grounding Lite) | Read only: places, routes, weather. Replies keep Google's Maps links, as its terms ask. |

**Slack, Notion, Linear** and anything else you connect stay on
[Composio](stackchan/telegram.md#the-apps-composio). Drive and GitHub are kept
**out** of the Composio session even if they are connected there
(`composio_tools.DIRECT_TOOLKITS`), so each app has exactly one way in.

**eve lite** has none of the direct apps: every app, the person's own Gmail and
Calendar included, goes through Composio Connect, signed in once in the browser. See
[eve lite: Composio Connect](#eve-lite-composio-connect).

**Your second Google account** (owner, 2026-10-05: "keep composio for the second
gmail"). Your **personal** Gmail and Calendar are the direct tools above. Your
**work** Gmail and Calendar stay on Composio, connected there under the alias
`work` (`composio_tools.OTHER_ACCOUNT_TOOLKITS`):

- Only an account with an alias other than `personal` (in any case, with
  spaces trimmed) joins Composio's session. An unaliased one cannot be told
  apart from your own, so it stays out. Gmail or Calendar named in
  `EVE_COMPOSIO_TOOLKITS` with no such account stays out too.
- The session then runs in explicit-account mode, so every Gmail or Calendar
  call through Composio names its account. A call naming no account, or naming
  `personal`, is refused and pointed at the direct tools. A workbench script
  cannot name its account, so one that calls Gmail or Calendar is refused.
- Work Gmail stays read only. Routine work-calendar changes run; invitations,
  deletion and guest delivery ask first, just as on the direct calendar.
- "Work email", "work inbox", "work calendar" and "era inbox" open Composio's
  tools for that turn (the toolset router's workspace keywords).

Owner's decision, 2026-10-04. A broker in the middle of your mail, calendar and
code is one more company holding your tokens and one more layer to debug. The
code is `bridge/src/eve_bridge/apps_mcp.py` (the bridge, the MCP client,
sign-in) and `google_rest.py` (Google's four).

Owner's decision, 2026-10-09: eve may **send** email and may **look up** Google
contacts. App defaults now use `auto`: ordinary private work runs, and sending,
deleting, publishing, sharing, purchases and unknown changes ask. Every email is your yes
first, and the question is the whole email, never a clipped line. Sending uses
Google's `gmail.send` permission, which can send and nothing else: it cannot
read, label or delete.

An email is never a Mac task while Gmail can send it. On 2026-10-09 "Email me a
test saying hi" went to Codex, which drafted it in the browser and asked twice.
Now a `start_task` whose goal sends email ("email Sam…", "send an email to…",
"reply to…") is refused by code before anything is asked, and the reason tells
the model to use `gmail__send_message` (`telegram.MAIL_SEND`). Screen tasks that
only open or look at mail ("open my email", "check my inbox") still start.

## Why Google's plain APIs and not its MCP servers

Google runs MCP servers for Gmail, Calendar and Drive, but they serve only a
Cloud project enrolled in the **Workspace Developer Preview Program**. That
program is for Workspace (paid) accounts. With a personal gmail.com account, live
on 2026-10-04, every call came back:

> Access to this tool requires that your Google Cloud project (…) is enrolled in
> the Google Workspace Developer Preview Program.

The same sign-in reads Google's regular APIs, so eve uses those. The tools keep
the MCP servers' names and arguments (`gmail__search_threads`,
`calendar__list_events`, `calendar__suggest_time`, `drive__search_files`, …), so
the brain reads the same either way.

## Set it up

All the files live in `~/.config/eve-bridge/`, owner-only (0600).

1. **Switch it on.** Add `EVE_APPS=1` to `~/.config/eve-bridge/env`.

2. **Google (Gmail, Calendar, Drive, Contacts).** Do this once, in a Cloud project of your
   own, signed in as your personal account:
   1. In [console.cloud.google.com](https://console.cloud.google.com), create a
      project.
   2. Under **APIs & Services**, enable the **Gmail API**, the **Google Calendar
      API**, the **Google Drive API** and the **People API** (contacts).
   3. Under **Google Auth Platform → Audience**, choose **External** and add your
      gmail as a **test user**. Then press **Publish app**. An app left in
      "Testing" has its sign-in expire after 7 days (Google's rule). Publishing
      your own unverified app needs no review; you only see an "unverified app"
      warning when you sign in. While it stays in "Testing",
      `eve-bridge refresh schedule` reminds you weekly ([the weekly refresh](refresh.md)).
   4. Under **Credentials → Create OAuth client**, pick **Desktop app**. Download
      the JSON as `google-oauth-client.json`.
   5. Run `eve-bridge apps login google`. The browser opens. On the
      unverified-app warning, choose **Continue**, then allow everything. eve
      listens on `127.0.0.1` for Google's answer, uses PKCE, and stores the
      refresh token in `google-token.json`. If you untick a permission, the
      command names what was not granted. A sign-in from before 2026-10-09 has
      no permission to send mail or read contacts: run the command again to add
      them.

3. **GitHub.** Create a
   [fine-grained token](https://github.com/settings/personal-access-tokens) for
   your repositories.
   - Give it **Contents**, **Issues** and **Pull requests** (read and write).
   - Add **Actions** (read) if you want "did CI pass?".
   - Put it in the env file as `EVE_GITHUB_MCP_TOKEN=…`.

   GitHub serves notifications only to classic tokens, so eve does not offer
   them.

4. **Google Maps.**
   1. Make an API key in a Cloud project.
   2. Enable the **Maps Grounding Lite API** (`mapstools.googleapis.com`) for that
      project.
   3. Restrict the key to that API.
   4. Put the key in the env file as `EVE_GOOGLE_MAPS_KEY=…`.

5. **Check and restart.** `eve-bridge apps status` lists each app: up or
   not, how it is reached (`rest` or `mcp`), its tool count and its policy. Then
   restart the daemon:
   `launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon`.

## Settings

| Variable | Default | What it does |
|---|---|---|
| `EVE_APPS` | `0` | `1` turns the direct apps on. Each app comes up only when its sign-in or key is present. |
| `EVE_APPS_POLICY` | `gmail=auto,calendar=auto,drive=auto,contacts=read,github=auto,maps=read` | `auto` runs supported routine private changes and asks for consequential or unknown changes. `write` has the same protection; it cannot bypass confirmation for sending or deleting. `read` refuses changes and hides their tools. `ask` asks for every change, including routine ones. Reads run. Moving Gmail or Calendar between `read` and a writable policy changes the requested Google permissions, so sign in again afterwards. |
| `EVE_APPS_TIMEOUT_SECS` | `60` | One call's timeout. |
| `EVE_APPS_DIR` | `~/.config/eve-bridge` | Where the Google client and token files are kept. |
| `EVE_GITHUB_MCP_TOKEN` | unset | GitHub's fine-grained token. |
| `EVE_GOOGLE_MAPS_KEY` | unset | The Maps Grounding Lite API key. |

## How a turn uses them

**Apps open on demand.** Offering every app's tools on every turn would add
roughly 30k tokens to each request; GitHub alone is about 44 KB of schemas. Most
turns use no app. So:

- A turn starts with the apps your message names, matched by words
  (`apps_mcp.APP_WORDS`). A request that needs two apps opens both, because a
  miss costs a whole extra round:

  | App | Words that open it |
  |---|---|
  | Gmail | mail, email, inbox, unread, newsletter, receipt, "sent me" |
  | Calendar | calendar, meeting, event, appointment, agenda, schedule, busy, "am I free", "free at", invite, RSVP, class, lecture, lab, section, seminar, office hours, exam, midterm, final, quiz, flight, "what's next", "up next", "my day" |
  | Drive | drive (not "drive to"), doc, Google Doc, document, sheet, spreadsheet, slides, folder, PDF |
  | GitHub | GitHub, repo, PR, pull request, commit, branch, CI, "the build", "build passed", workflow, "actions run"/"actions failed"; "issue" only when the message names no other home for it (Linear, Notion, Slack, Jira, an order) |
  | Maps | map, maps link, directions, navigate, route, how long, how far, get there, drive/walk/bike to, commute, traffic, ETA, nearby, near me, nearest, where's, where is, address (not "email address"), location, weather, forecast, rain, restaurants, cafés, coffee/food near |

  So "look for my next class on my calendar and give me a maps link" opens the
  calendar and Maps at once, and "where's my next lecture" does too. Bare
  "today" or "tomorrow" opens nothing (they ride along with most requests), and
  "finally" is not "finals". A bare "build" does not open GitHub: "build me an
  app" is eve's app maker, and GitHub's tools are the largest.
- The brain also gets `open_app`, a small tool that loads more apps for the
  rest of the turn. It takes a list, `{"apps": ["calendar", "maps"]}`, so every
  app a request needs opens in one call. The older `{"app": "calendar"}` still
  works. An app that is not connected is reported in the answer
  (`not_connected`) and the others still open.
- The morning `rundown` opens Gmail and the calendar from the start.

**Calendar events carry Maps links.** Every event the calendar tools return
(`list_events`, `search_events`, `get_event`, `create_event`, `update_event`,
`quick_add_event`, `respond_to_event`) that has a place in its `location` gets
two links, built by
[Google's Maps URLs](https://developers.google.com/maps/documentation/urls/get-started)
scheme (`api=1`, the place URL-encoded, no key needed):

- `mapsUrl`: `https://www.google.com/maps/search/?api=1&query=<place>`
- `directionsUrl`: `https://www.google.com/maps/dir/?api=1&destination=<place>`,
  which starts from where your phone is.

A location that is a link (a Zoom or Meet URL), a bare Meet code, or the name of
a call ("Microsoft Teams Meeting", "Online") gets no links. So "my next class
and a maps link" is one calendar call, with no Maps call at all.

**Events are compact.** The model gets each event's `id`, `status`, `summary`,
`start`, `end`, `location` and its links, the `description` clipped to 500
characters, `hangoutLink`, `conferenceData`, the guests (email, response, self,
organizer), the organizer's email, `recurringEventId` and `htmlLink`. Google's
etag, creator, reminders and the rest are dropped. A call link (Meet, Zoom,
Teams, Webex) past the description's clip is kept at its end, so Meet's own
calendar lookup (`meet.event_link`) still finds it.

**How the brain is told to use them** (`AppsBridge.instructions`, a few lines
on every turn while an app is up):

- A job inside your own mail, calendar, Drive, contacts, GitHub, or places,
  routes and weather uses these tools, which answer in seconds, rather than a task on the
  Mac's screen or a web search.
- Open every app a request needs in one `open_app` call.
- Chain apps in the same turn without asking in between. "My next class" is
  `calendar__list_events` from now with no `calendarId`, which reads every
  calendar you see in one call; its event already carries the Maps links.
  The Maps tools are for travel time, nearby places and weather.
- Keep the links that results carry in the reply.
- Gmail sends email in your voice; you see each one whole and say yes by code.
  An answer to a message goes into its thread.
- An email address or phone number the brain does not have comes from Google
  Contacts; it never guesses an address.

Each line about an app's tools appears only while that app is up, so the brain
is never told about a tool it does not have.

**For the chat loop** (`telegram.py`), the bridge provides these operations:

- `owns(name)`: is this name the bridge's? True for `<app>__<anything>` of an
  app that is up, and for `open_app` while any app is up.
- `ensure_open(name, opened)`: open the app of a name the model called, for the
  rest of the turn.
- `prepare(name, args)`: copy the arguments. For an ordinary direct calendar edit
  with no notification choice, read the existing event through Google's own REST
  adapter. If the full record verifies no guests, pin `notifyGuests: false` so the
  edit stays silent even if someone adds a guest before it executes. Explicit
  notification choices and `read`/`ask` policies are preserved; failed or incomplete
  reads leave the call unchanged for confirmation.
- `decide(name, args=None)`: run, ask or refuse. Routine classification needs
  the actual arguments; an older caller that omits them still asks for writes.
  A name an up app does not have (made up,
  or a change a read-only app never offered) is refused with that app's real
  tool names, so the next round can call the right one.

The chat loop accepts any name `owns` claims, even one this turn did not offer
(`telegram.AllowedNames`). Such a call runs through `ensure_open` and `decide`
like any other: "what's on for me?" followed by `calendar__list_events` runs the
call and offers the calendar's tools from the next round, and a made-up
`<app>__<tool>` gets the refusal back as its result. Neither fails the turn.

A name that nothing claims (an app that is not up, or a Composio tool while
Composio is off) is refused the same way: the call's result says it is not a
tool here and lists the tools the turn has, and the next round goes on. Before
2026-10-05 such a call failed the whole turn. The Composio block names the
direct apps' tools only for the direct apps that are up, and the rundown skill
names only the sources that are present this turn.

**Schemas are slimmed.** Output schemas, vendor extensions and deprecated fields
are dropped, and descriptions are clipped. Calendar's raw MCP list is about
130 KB, mostly output schemas the model never reads.

**What runs is decided by code** (`AppsBridge.decide`):

- A call counts as **reading** only when both of these hold:
  - the server marks the tool `readOnlyHint`;
  - its name has no writing verb.

  So a mislabelled `trash_…` still counts as a change, and a tool with no
  `readOnlyHint` is a change too (2026-10-05: the name alone let
  `find_and_replace` run as a read).
- A **change** follows its app's policy and a closed list of supported private
  operations. New files in the owner's Drive root, private draft documents, unsent
  drafts when offered, explicitly private personal repositories, and calendar
  changes without delivery can run under `auto` or `write`. Unknown arguments,
  existing/shared file destinations, permission changes and remote code ask.
  A batch takes the strictest verdict of every call, including repeated tool names
  with different arguments. Work Gmail's explicit `read` policy stays in force.
- Calendar creation with attendees asks even if notifications are disabled,
  because adding someone shares the event. Editing an existing direct event
  runs unasked with `notifyGuests: false`; Composio uses `send_updates: "none"`.
  For direct edits that omit the control, code first checks the complete existing
  event and sets false only when there are no guests. Guest-bearing events,
  omitted attendee metadata, read failures and unknown adapters still ask.
- An **ask** shows the whole operation and its arguments with Allow and Deny buttons.
  An operation too long to show is refused, never approved from a clipped preview.
  It is firm:
  only a tap, or a bare yes or no typed, answers it. A sentence such as "ok, and
  what's the weather?" goes on as a new message and approves nothing.
- **An email is asked whole** (`apps_mcp.WHOLE_ASKS`): "Send this email?", then
  To, Cc, Bcc and Subject, then the body as it will go, line breaks kept. Hidden
  and direction-changing characters are written out as `[U+XXXX]`. An email too
  long to show (over 3,500 characters) is refused without asking, and the brain
  is told to have you shorten it. Nothing is clipped.

## What the Google tools do

These are Google's own APIs, checked against Google's reference on 2026-10-04.

- **Gmail** (`gmail.readonly`, and `gmail.send` unless Gmail is `read`):
  - `search_threads` takes Gmail search syntax and pages with `pageToken`.
  - `get_thread` and `get_message` return the text and list the attachments. A
    `.txt` attachment is never mistaken for the body.
  - `get_attachment` reads a PDF or text attachment.
  - `list_labels` returns the inbox's total and unread counts.
  - `send_message` sends plain text to `to`, `cc` and `bcc` (20 addresses at
    most). Each must be exactly an email address: a name, or a value with a line
    break that would start another header, is refused, never trimmed. With
    `replyToMessageId` the email goes into that message's thread
    (`threadId`, `In-Reply-To` and `References` from the original) and the
    subject defaults to "Re: …". Gmail fills in From. "me" in any of them is
    your own address, read from Gmail's profile only when it is used.
- **Contacts** (`contacts.readonly` and `contacts.other.readonly`):
  - `search_contacts` searches your saved contacts and the people you have
    emailed but never saved, by name, email or phone. It returns names, email
    addresses, phone numbers and organizations; one address found in both is
    one person. Google asks for an empty search first to warm its cache, so the
    first search of a run makes one.
  - Contacts open by their words alone ("Ana's number", "email Sam that …",
    "her email address"); the toolset router asks Jev nothing about them
    (`toolset_router.KEYWORD_ONLY`).
- **Calendar** (`calendar`):
  - `list_events` shows recurring events as single occurrences, soonest first.
    With no `calendarId` it reads every calendar you see except holiday,
    birthday and week-number calendars (up to 20), merges them, and tags each
    event with its calendar's name. A calendar that cannot be read is named in
    `unreadable`; the rest still come back. A `calendarId` reads that one only. A
    time with no timezone is taken as the Mac's own, because Google refuses one
    without an offset.
  - `search_events` searches from a year back.
  - `suggest_time` finds free slots in working hours across the calendars you own,
    using freebusy.
  - `create_event` accepts recurrence rules and can add a Meet link.
  - `quick_add_event` takes one sentence: "Lunch with Ana Friday 12pm".
  - `update_event` can switch an event between all-day and timed, and adds guests
    to the ones already there.
  - `respond_to_event` sends your yes, no or maybe and keeps every other guest
    unchanged.
  - `delete_event` deletes an event.
  - Guests are emailed about a change when the event has guests (the API's
    own default is never), unless `notifyGuests` is false. Such delivery asks first.
- **Drive** (`drive.readonly` and `drive.file`):
  - `search_files` and `list_recent_files` page with `pageToken`.
  - `read_file_content` returns Docs as Markdown (headings and tables kept),
    Sheets as CSV and Slides as text. PDFs are read with macOS's own PDFKit
    (pyobjc Quartz, already a dependency), so no PDF library is added.
  - `create_file` creates a new private file in your Drive root without asking
    under `auto` or `write`.

## Your GitHub repositories, by name

`github_repos.py` (2026-10-09). You call your projects by what they are ("my car repo", "the scam detector"),
not by their names. With no repository word in a request, the brain could not tell a GitHub project from a Linear
one, a note or an app eve built. Two rounds of wording changes failed their blind tests (19/26 and 17/26
against a bar of 24). So eve now knows your repositories themselves.

- **Where from.** When the apps start and GitHub is up, eve asks GitHub's own server who you are (`get_me`) and
  lists your repositories (`search_repositories`, `user:<you>`, most recently pushed first, up to 100), private
  ones included as your token sees them.
- **Kept.** `~/.config/eve-bridge/github-repos.json`, owner-only (0600), refreshed once a day. A failed
  refresh keeps the last list.
- **Shown.** The 25 most recently pushed, each as `name (what it is)` with the description clipped to 50
  characters, in one line of the apps block. Every name there is one GitHub returned.
- **As words.** A repository's own name in a message (`zorbcar`, `plantpal-x`) opens GitHub like any GitHub word.
  Names under 4 characters, and names that are everyday words (in macOS's `/usr/share/dict/words` or a short
  list in the module), never do, so "remix" in a sentence opens nothing.
- **Privacy.** The names and descriptions ride the brain's prompt, which goes to the model provider like the
  rest of the turn. Logs carry counts only.

## eve lite: Composio Connect

[eve lite](lite.md) has no direct apps: no Google Cloud client, no Google key, no GitHub
token (the owner, 2026-10-13: "the google env key setup is unnecessary for this variant").
Its one way to any app is **Composio Connect**, Composio's hosted MCP server for a person's
own agent, signed in once in the browser. Spike S1 ([its notes](plans/eve-lite-s1.md),
2026-10-13) checked that sign-in against Composio's live services; a full sign-in by a
person is manual gate M5, still pending.

**The sign-in** (`onboarding.ComposioConnectLogin`): OAuth with PKCE, as the MCP
authorization spec describes it.

- eve registers itself once as a public client (dynamic client registration, RFC 7591, with
  no secret), named "eve lite on <this Mac>", with the two loopback forms of its callback on
  the port the Mac's page has: `http://localhost:<port>/onboard/composio/callback` and the
  same on `127.0.0.1`. It registers again only when the page moved to a port the stored
  registration lacks, or when Composio no longer knows the client.
- The browser goes to `https://connect.composio.dev/oauth/authorize` with S256, a
  single-use `state`, the scope `openid profile email offline_access` and
  `resource=https://connect.composio.dev/mcp`, so the token is good at the MCP server. The
  person signs in or signs up, then clicks **Allow**.
- The callback is checked by its `state` alone, and the code is traded at
  `https://login.composio.dev/oauth2/token` with a form POST and no secret. The token
  endpoint's host is not the issuer's, so every endpoint is fixed in the code, not worked out
  from the issuer.
- `~/.config/eve-bridge/composio-connect.json` (mode 600) gets the client id, the callbacks,
  the tokens and when the access token lapses. It is written only once a token arrived, so
  the file being there means "signed in", and the daemon restarts into it.

**The bridge** (`composio_connect.ConnectBridge`, the same shape the brain reads full eve's
Composio bridge in):

- It lists Connect's seven meta tools over MCP and offers them under their own names, in
  this fixed order: `COMPOSIO_SEARCH_TOOLS`, `COMPOSIO_GET_TOOL_SCHEMAS`,
  `COMPOSIO_MULTI_EXECUTE_TOOL`, `COMPOSIO_MANAGE_CONNECTIONS`,
  `COMPOSIO_WAIT_FOR_CONNECTIONS`, `COMPOSIO_REMOTE_WORKBENCH` and
  `COMPOSIO_REMOTE_BASH_TOOL`. Any other tool the server lists is left out. Schemas are
  slimmed. Composio's descriptions are kept whole, because they carry how the meta tools are
  used, less the first line of `COMPOSIO_SEARCH_TOOLS`', which advertises toolkits eve never
  runs.
- The access token is refreshed about a minute before it lapses, and once after a 401, and
  the call is then tried once more. A refresh token Composio refuses is never sent again: the
  person signs in again from **Settings → Apps**. A call never raises; a failure is one plain
  line for the model. A call that timed out is not tried again, since it may have run.
- `start()` lists the tools on a thread of its own and tries again, further apart each time,
  until Composio answers. Until then the brain offers no app tools.

**Connect Links.** **Settings → Apps** lists Gmail, Google Calendar, Google Drive, Slack,
Notion, Linear and GitHub, each with whether it is connected (read with
`COMPOSIO_SEARCH_TOOLS`, which only looks) and a **Connect** button. **Connect** calls
`COMPOSIO_MANAGE_CONNECTIONS` for that app and opens the sign-in link it returns; only an
`https` link on `composio.dev` or a subdomain of it reaches the browser. In the chat, a
request for an app that is not connected gets the same link from the brain, which checks the
connection once the person says it is done. Any other toolkit with Composio's own sign-in
connects the same way.

**No personal-account split.** Full eve reaches the owner's own Gmail and Calendar directly,
and refuses a Composio call that reaches them ([at the top](#connected-accounts-direct-and-composio-for-the-rest)).
eve lite has no direct apps, so `composio_tools.decide_lite` judges each call with
`decide(..., personal_direct=False)`: the person's own mail and calendar go through Composio.
Every other rule is full eve's:

- Reads run.
- Gmail drafts and routine private calendar and Drive edits run (`EVE_COMPOSIO_POLICY` is
  forced to `gmail=auto` in eve lite; Calendar and Drive are `auto` already).
- Sending, replying to or forwarding an email asks, and the question shows the whole email,
  with the room full eve gives a Gmail send (`composio_tools.sends_mail`). Deleting and every
  other one-way act ask, showing the whole act. The remote bash tool always asks. A remote
  workbench script asks (`composio_tools.workbench_reaches_out_lite`) when it reads the
  person's own mail or calendar, or when it could carry data off the Mac through a web-search
  or scraping tool called in it. A pandas or numpy reader that fetches a URL asks in full eve
  too (`composio_tools.workbench_slugs`). A script that only computes on results already
  fetched runs.
- **A connection change asks.** Full eve runs `COMPOSIO_MANAGE_CONNECTIONS` without asking.
  eve lite asks unless `composio_tools.manage_needs_yes` proves the call only checks or links:
  `toolkits`, a non-empty list of names; `reinitiate_all` absent or false (true replaces live
  connections); a `session_id` string or none; and no other key. Any other shape asks, a
  remove or a rename included.

The Lean model `Eve/AppsPolicy.lean` proves both halves: with `personal_direct` true,
`decide` is today's for every call; and in lite, every write that is not routine, every code
tool and every connection change `manage_needs_yes` does not prove runs only after a whole
question and a bare yes ([verification](verification.md#eve-lite-2026-10-13)).

### If Composio Connect fails: the contingency

The contingency is **not built**. S1 found that Connect takes eve's registration, so it is
built only if manual gate M5, after a real sign-in, shows one of these: Connect refuses the
loopback callback or never comes back to it; the token endpoint refuses the code or the
refresh for a public client; `/mcp` refuses the token; or its tool list lacks the meta tools.
A changed tool schema is not one of them: the fixtures are refreshed from Connect instead.

It would be Composio's CLI-session login (WP-6b in [the spec](plans/eve-lite.md)). eve asks
Composio's backend for a session, the browser opens Composio's dashboard to approve it, and
eve polls the session by its UUID only, never its short code. It gets a project API key: an
`ak_` key is used as it is, and a `uak_` user key is traded once for a project key. The key is
saved as `COMPOSIO_API_KEY` in the env file, and the brain uses full eve's Composio bridge in
a lite subclass with no personal-account split, as the user `eve-<uuid>`. Its costs: the
composio SDK, which needs `pysher`, which has no wheel, so a release would ship one;
`COMPOSIO_API_KEY` added to the one secret eve lite keeps; and httpx logging held at WARNING,
so the session id never reaches a log.

## Privacy

- **Tokens.** No token is ever logged, repr'd or shown to the model. The token
  files are 0600.
- **Request logs.** httpx logs every request line with its URL, and a Google URL
  carries your words (`q=from:sam`). Those lines are dropped for the apps' hosts
  (`apps_mcp.APP_HOSTS`); every other host still logs.
- **Errors.** Logs carry tool names and exception types, never arguments, mail
  text or results.

## Tests

- `bridge/tests/test_routine_app_policy.py` checks private writes against sends,
  deletion, sharing, ignored safety aliases and mixed batches. Its real REST
  request test proves routine calendar edits send no guest updates, with a
  positive control that does send updates when the delivery control is omitted.
  Calendar preparation tests cover a guest appearing between inspection and
  execution, incomplete attendee metadata, failed reads and strict owner overrides.
- `verification/Eve/AppsPolicy.lean` models the current `auto`/`write` decision,
  the calendar delivery boundary, strict owner overrides and whole-act approval.
  Provider argument classification is checked in Python; the Lean model states
  that boundary explicitly. The prompt change has a separate
  [blind evaluation](evals/routine-confirmations-2026-10-09/BAR.md).
- `bridge/tests/test_apps_mcp.py`:
  - The opening words (compound requests and negative controls), `open_app`
    with several apps, `owns`, `ensure_open`, and refusals that name the real
    tools.
  - Maps links on events, compact events that Meet still reads, and every
    calendar call that returns events.
  - The policy, schemas, `open_app` and the MCP client, over a mock transport:
    the session, SSE, a forgotten session reopened, the agreed protocol version
    and API-key headers.
  - Token refresh.
  - Each Google call's parameters.
  - A real PDF read through PDFKit.
- `bridge/tests/test_telegram.py::test_direct_app_tools_are_opened_by_the_message_and_executed_through_the_bridge`
  drives the whole chat path.
