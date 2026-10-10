# OpenTable restaurant search

`/opentable` and `opentable_search` start a read-only search in the owner's
existing signed-in OpenTable Chrome session. Eve uses its existing Codex
computer task and installed `cua_repl` browser tools to inspect restaurant
profiles and available times. Results arrive through the normal task progress,
questions and final delivery. This is a best-effort browser route, not an
OpenTable API partnership or a reservation service.

The [local contract](contracts/opentable.json) describes Eve's tool request and
immediate launch receipt. It creates no REST endpoint and promises no structured
upstream availability response. Final observations are the existing agent's text
and, when available, an actual task screenshot.

## Enable and use

Add this setting to `~/.config/eve-bridge/env`, then restart Eve:

```dotenv
EVE_OPENTABLE=1
```

The setting defaults to off and accepts `1`, `true`, `yes` or `on`, ignoring case
and surrounding spaces. Existing computer control must also be enabled
(`EVE_COMPUTER_CONTROL`), and the computer-agent factory must be available.
The owner-scoped menu entry is “Find restaurants and available tables”. Normal
Chrome, its existing sign-in, and the installed Codex browser tools are required
for the task to operate. The feature adds no OpenTable credential, API
subscription, Apify key or Firecrawl query. Existing computer-agent model usage
and account limits still apply.

For example:

```text
/opentable Italian restaurants in Seattle tomorrow at 7 pm for 2
```

The command accepts an optional `@BotName` suffix and is case-insensitive.
A bare `/opentable` shows an example without launching a task. Arguments go
directly to the browser task ahead of Claude/Codex relays and leave unrelated
pending questions intact. `/opentable cancel` stops only the currently running
OpenTable-owned task in that owner chat; it does not cancel a reservation or
another computer task. The task's normal Stop control remains available.

The same shared text/call brain can call `opentable_search` for a natural
OpenTable request. Only configured owners may launch it. Requests have exactly
one field, a nonblank string of at most 4,000 characters:

```json
{"request": "Italian restaurants in Seattle tomorrow at 7 pm for 2"}
```

Wrong types, blank strings, oversized input, invalid Unicode or extra fields are invalid.
The module validates length before trimming the request. An unavailable
feature, denied caller, occupied computer or invalid request starts no search
and triggers no alternate provider or paid discovery query.

## What the task observes

The task asks for a missing city/location or specific restaurant, local date,
local time and party size. It does not choose a destination from geolocation,
an IP address, saved residence or browser defaults. Each goal includes a checked
aware local clock snapshot, with its offset and timezone. That clock is not the
restaurant's timezone; relative dates require current clock and destination
context, with clarification when ambiguous.

Before reading slots, the task checks the visible search controls against the
confirmed filters. The final result should report those exact filters, observed
restaurant/profile links, actual displayed times, visible seating/experience
constraints, and a fresh observation time. A timezone is attributed to a slot
only when visible; unknown values stay unknown. Links are copied from the
browser, with no invented affiliate or deep-link parameters.

An observed empty availability result differs from an unloaded page, blocked
access or missing input. Search snippets and restaurant profiles alone do not
prove availability. Listed times can change before the owner opens a link.

The fixed goal permits searching and opening profiles, and prohibits selecting
a slot, holding a table, submitting/modifying/cancelling a reservation, filling
booking identity/payment fields, paying or contacting a restaurant. It also
prohibits hidden/internal endpoints, cookie/credential extraction, challenge
bypass, profile/account changes, rewards enrollment and new terms acceptance.
Authentication, card requirements, challenges or unsupported flows stop the
search with an honest explanation. A later booking request belongs to a
separate workflow with review of concrete details and any required confirmation.
It cannot expand this search task into a reservation.

These are instructions to the existing general computer agent, not a new
transport that technically rejects all website mutations. The module's unit
tests verify validation, schema and goal wording; an actual browser run is
needed to establish observed availability or an observed blocker. No guaranteed
unattended success, measured speed, booking or reservation confirmation follows
from those unit checks.

## Local launch receipt

A successfully launched tool call returns:

```json
{
  "ok": true,
  "status": "started",
  "provider": "opentable_browser",
  "reservation_submitted": false,
  "note": "Search started; observed availability and links will be sent when it finishes. No reservation has been made."
}
```

This receipt contains no availability, restaurant result, reservation ID or
confirmation. The existing progress message acknowledges the task; the chat
brain needs no extra model call solely to acknowledge a successful launch.
Final browser observations arrive separately, after the task checks the UI.

Failures use `ok: false`, `status: unavailable` or `invalid_request`, and a
`reason`. An off feature also returns `opentable_url`; computer launch/busy
failures may include `reservation_submitted: false`. Those optional fields are
not promised on every failure. No failure means that a table was unavailable or
that a reservation was cancelled. An opened page, Reserve control, available
time or started task is never a booking confirmation.

## Access limits

Discovery on October 4, 2026 found no OpenTable capability among the inspected
Alexandria and Composio results. That is a bounded observation, not universal
proof of absence. OpenTable does publish [partner API documentation](https://docs.opentable.com/),
but that path requires approved partner access and an applicable agreement; no
consumer availability endpoint or Eve API entitlement was established.
OpenTable's [OpenAI partnership](https://www.opentable.com/restaurant-solutions/resources/openai/)
does not confer that entitlement on this integration.

OpenTable's [Terms of Use](https://www.opentable.com/c/legal/terms-and-conditions/),
revised July 22, 2026, §13 restricts AI assistants and other automated access
unless OpenTable expressly authorizes it, and restrict seeking nonpublic APIs.
An existing sign-in alone does not establish provider authorization. Eve's
opt-in browser feature does not claim an approved partnership or override these
access limits. The website may refuse access, and UI changes may break a search.
The contract adds no provider terms-acceptance flow or fabricated API approval.

## Observed verification

On October 4, 2026 at 1:50 PM PDT (UTC−07:00), the production Codex agent
completed the actual module's browser goal for Seattle, October 5, 2026 around
7 PM, two people, with no preference filters. The saved screenshot shows those
search controls and these displayed slots:

| Observed restaurant profile | Displayed times |
|---|---|
| [Momiji – Capitol Hill](https://www.opentable.com/r/momiji-capitol-hill-seattle) | 6:45 PM, 7:00 PM, 7:15 PM |
| [Bar Cantinetta – Capitol Hill](https://www.opentable.com/r/bar-cantinetta-capitol-hill-seattle) | 6:45 PM, 7:00 PM, 7:15 PM |

The cards displayed no timezone label or seating/experience constraints; PDT
was part of the explicit request and checked clock context. No slot was selected
and no reservation was made. This establishes one observed browser search, not
continued availability, guaranteed future access, a live Telegram/call turn or
installed-daemon enablement. Runtime activation was verified separately below.

On October 4, 2026, Eve's private env was enabled for OpenTable with mode
`0600`, preserving the other settings. The daemon restarted successfully, its
IPC responded, and its import resolved to this editable checkout. A read-only
owner-menu query returned exactly one `/opentable` entry with “Find restaurants
and available tables”; `/america` remained registered. This verifies the
installed software and menu, without a live Telegram/call turn or reservation.
