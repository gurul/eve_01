# Canvas

eve can read your [Canvas LMS](https://www.instructure.com/canvas) from the
[Telegram chat](telegram.md): your current courses, what is due, course
announcements, and links to assignments, pages, files and modules. It is
**read only**: it cannot submit, post, comment or change anything on Canvas.

Asked for on 2026-09-30 ("build a canvas connector"). The code is
`bridge/src/eve_bridge/canvas.py`; the tests are `bridge/tests/test_canvas.py`.

## Turn it on

1. **Make a token.** In Canvas, open **Account → Settings**, scroll to
   **Approved Integrations**, and press **+ New Access Token**. Give it a purpose
   (e.g. "eve") and, if you like, an expiry date. Copy the token: Canvas shows it
   only once.
2. **Put two lines** in `~/.config/eve-bridge/env` (the file is yours, mode
   600; values there are never logged):

   ```sh
   CANVAS_BASE_URL=https://<school>.instructure.com
   CANVAS_API_TOKEN=<the token you copied>
   ```

   `CANVAS_BASE_URL` is the address you open Canvas at, e.g.
   `https://<school>.instructure.com` or your school's own domain such as
   `https://canvas.<school>.edu`. It must be `https://`. A pasted course page
   address or a trailing `/api/v1` is trimmed to the site.
3. **Restart eve** (the daemon reads the env file at start).

Either setting missing: Canvas is off and the brain gets no Canvas tools.
`EVE_CANVAS=0` turns it off with both set.

When the token expires or you delete it, eve says so and tells you to make a
new one (step 1), then replace `CANVAS_API_TOKEN` and restart.

### When the school blocks personal tokens

Some schools turn off **New Access Token**. In that case, use Canvas's own
calendar feed instead. Click **Calendar → Calendar Feed** and copy the private
`.ics` link. In Google Calendar, use **Other calendars → + → From URL** and add
the link. eve reads every Google calendar
(`GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS`), so assignments show up as events
at their due time, with no Canvas settings. Set up 2026-10-02.

The feed gives less than the token does:

- **No submitted status and no announcements.** The feed carries neither.
- **Slow updates.** Google refreshes a subscribed feed every few hours to a day.
- **The rundown shows today only.** It lists today's assignments, not the
  next three days'.

The link is a secret: anyone who has it can read your calendar.

## What you can ask

| You text | Tool | What comes back |
|---|---|---|
| "what classes am I in" | `canvas_courses` | current courses: name, code, term |
| "what's due this week", "anything due in 447?" | `canvas_due` | assignments due in the next days (default 7, up to 60), each with its due time in your Mac's time zone, submitted or not, points, link; unsubmitted work up to a week overdue is shown as missing |
| "any announcements in 333?" | `canvas_announcements` | the last 14 days' announcements (or as many days as you ask), newest first, as plain text with links |
| "send me the link to HW3", "the A3 starter files" | `canvas_find_link` | matching assignments, pages, files and modules in that course, each with its link (a module lists its items) |

A course is named by its code ("CSE 447", "cse447"), part of its name, or its id.
If two courses match, eve asks which.

**The rundown.** With Canvas on, `rundown` (and "plan my day") also lists
unsubmitted work due in the next three days under Todos, as "Due on Canvas".
These are read by code before the rundown turn, so the rundown's own tools stay
email, calendar and Slack reads only. Without Canvas the rundown is unchanged.

**Self context.** While Canvas is on, eve's "live setup" block says
"Canvas (school courses): on, read only.", so asking eve whether it can see
Canvas is answered from what runs, not from memory.

## Why first party, not Composio

Composio is eve's pattern for the long tail of app suites (Slack, Notion,
Linear) where one session reaches many apps and a write needs a policy; the
everyday apps (Gmail, Calendar, Drive, GitHub) go direct ([apps.md](../apps.md)). Canvas is one
narrow, read-only need, so `canvas.py` follows the house pattern for single
services instead (`spotify.py`, `lights.py`): its own module, its own four
tools, handed to the brain with an `instructions()` block.

- **Control.** The four tools are a fixed list the brain sees on every turn. With
  Composio the brain first searches for Canvas tools, reads their schemas, then
  executes: two extra model rounds for "what's due".
- **Read only by construction.** Every request goes through `ReadOnlyTransport`,
  which refuses anything but GET before it reaches the network. Composio's Canvas
  toolkit has write tools that eve would have to police by slug.
- **One party.** The token goes from your Mac to your school's Canvas and nowhere
  else. Through Composio it would sit with a third party, behind a per-school
  OAuth app your school would have to register.
- **Reliability.** No SDK, no session to resume; `httpx` is already a
  dependency. Answers are shaped for a phone (local times, submitted or not,
  links) rather than raw API rows.

## Privacy

- **Read only.** GET requests only, enforced in the transport; tested with a
  positive control (a POST, PUT, DELETE or PATCH is refused and never reaches
  the transport).
- **The token stays on the Mac**, in `~/.config/eve-bridge/env`. It is
  sent only in the `Authorization` header, only to `CANVAS_BASE_URL`'s host:
  a pagination link that points at any other host is not followed. It never
  appears in a URL, a log line or a `repr`.
- **Logs** carry tool names and status codes only, never course content.
- **File links** are the file's page in Canvas (you log in to open it), never
  its download address, which can carry a verifier that opens the file for
  anyone holding the link.
- Announcement text is passed to the brain as data, not instructions.

## How it reads Canvas

Endpoints used (Canvas REST API, [canvas.instructure.com/doc/api](https://canvas.instructure.com/doc/api/), read 2026-09-30):

| Endpoint | Used for |
|---|---|
| [`GET /api/v1/courses?enrollment_state=active&include[]=term`](https://canvas.instructure.com/doc/api/courses.html) | current courses; date-restricted, completed and past-term courses are dropped |
| [`GET /api/v1/courses/:id/assignments?include[]=submission&order_by=due_at`](https://canvas.instructure.com/doc/api/assignments.html) | what is due, with your submission (submitted, late, missing, graded, excused) |
| [`GET /api/v1/announcements?context_codes[]=course_:id&start_date=&end_date=&active_only=true`](https://canvas.instructure.com/doc/api/announcements.html) | announcements |
| `GET /api/v1/courses/:id/assignments?search_term=` | assignment links |
| [`GET /api/v1/courses/:id/pages?search_term=`](https://canvas.instructure.com/doc/api/pages.html) | page links (`/courses/:id/pages/:url`) |
| [`GET /api/v1/courses/:id/files?search_term=`](https://canvas.instructure.com/doc/api/files.html) | file links (`/courses/:id/files/:id`) |
| [`GET /api/v1/courses/:id/modules?include[]=items&search_term=`](https://canvas.instructure.com/doc/api/modules.html) | module links and their items |

- **Pagination** ([docs](https://canvas.instructure.com/doc/api/file.pagination.html)):
  `per_page=50`, then the `Link` header's `rel="next"` URL, treated as opaque,
  up to 10 pages per list. The token is not in those links; it rides the header.
- **Rate limits** ([docs](https://canvas.instructure.com/doc/api/file.throttling.html)):
  requests are sequential, which Canvas says is unlikely to be throttled. Paging
  stops early when `X-Rate-Limit-Remaining` drops under 50. A throttled answer
  (429, or 403 "Rate Limit Exceeded") becomes "try again in a minute".
- **Errors.** 401 for the token: the new-token message. Canvas also answers 401
  "user not authorized to perform that action" for a part you cannot see; that,
  403 and 404 skip that course or that kind of item and name it
  (`could_not_read`), and the rest of the answer still comes back. Each request
  times out after 10 s.
- **Times.** Canvas answers in UTC; every time shown is in the Mac's local time
  zone, with the zone named ("Fri Oct 2, 11:59 PM PDT").
