# Government and legal questions with `/america`

`/america` and `america_ask` use Alexandria's government index by default.
Eve retrieves official sources, then its existing shared chat/call brain
explains the relevant material with citation links. The tool returns a source
bundle with `status: sources_found` and `provider: firecrawl_gov_index`.
This is not an America.gov-generated answer. It provides information, not legal
advice or a filing service.

The [Alexandria contract](contracts/america-alexandria.json) is a local JSON
Schema for the source bundle. It also records the tool's arguments as metadata.
It does not create an Eve REST server or invent an upstream HTTP endpoint.
The preserved [America.gov OpenAPI](contracts/america-gov.openapi.json) describes
only the experimental, explicitly selected `browser`/`api` transports.

## Configuration

Add to `~/.config/eve-bridge/env`, using the existing Firecrawl credential,
then restart the daemon:

```dotenv
EVE_AMERICA=1
EVE_AMERICA_TRANSPORT=alexandria
```

`FIRECRAWL_API_KEY` must already be configured. Alexandria needs no Chrome setup
or Jev control discovery. The tool is off by default; when enabled, its transport
defaults to `alexandria` even if that setting is omitted. Only configured owners
can use it. Existing Eve chat/Telegram settings apply; the separate wake-word
voice backend does not advertise it.

| Setting | Default | Effect |
|---|---|---|
| `EVE_AMERICA` | off | Adds `america_ask` and the owner-scoped `/america` menu command. |
| `EVE_AMERICA_TRANSPORT` | `alexandria` | Government-source retrieval. Explicit `browser` or `api` selects legacy America.gov chat. |
| `FIRECRAWL_API_KEY` | existing configuration | Authenticates the index call; never sent to source-page hosts. |
| `EVE_FIRECRAWL_PER_DAY` | `5` | Existing shared local-day Firecrawl call cap; `0` stops all Firecrawl calls. The saved count survives restart. |
| `EVE_FIRECRAWL_USD` | unset | Existing per-credit price for spend accounting; unset usage is recorded as unpriced. |

One paid index call shares the same cap as other Eve Firecrawl work. The
catalog observed on October 4, 2026 lists two credits for
`firecrawl-gov-index/search`; reported usage is metered under the `america`
feature. The adapter adds no separate synthesis service. Eve's existing
chat/call brain produces the final response.

## `/america` in the menu

Tap `/america`, then send the question with its state/city or say U.S. federal.
The owner-menu description is “Search U.S. law and government sources”. You can
also send the command and question together:

```text
/america Where can I read the official United States Code? This is U.S. federal.
```

The command reaches Eve while a Claude or Codex relay is active. Other pending
questions remain pending. Repeating a waiting command replaces only its America
prompt; repeating a running question reports that it is busy. `/america cancel`,
a cancel reply or `/stop` stops waiting without resending. A prompt timeout sends
no question. Attachments are unsupported.

Eve asks for missing jurisdiction instead of using a saved residence. The
prompt itself needs no model/provider call; its answer runs through the shared
chat brain. Retrieved text is source material, never instructions. Eve cites
original pages beside supported claims and makes missing or conflicting evidence
clear. It must check jurisdiction, effective dates and applicability before
giving deadlines or case-specific conclusions.

## Government-index retrieval

The fixed capability is `firecrawl-gov-index/search`, invoked through the existing
`firecrawl.run_tool` adapter. Each query contains only the explicit question and
jurisdiction, with `top_k: 5` and `full_content: false`. There is no catalog
discovery, general planner or browser action per question.

Eve canonicalizes and deduplicates official HTTPS `.gov`/`.mil` URLs before
reading at most three unique pages. It removes observed `;jsessionid` and
tracking/session variations while keeping meaningful query arguments. The
existing guarded HTTP reader rejects malformed, credential-bearing, private or
nonofficial targets and never shares Firecrawl authorization with page hosts.
No history, memory or attachments enter the index query.

Each source has an indexed `excerpt` of at most 2,000 characters and `page_text`
of at most 6,000 characters. `text_truncated` marks clipping. Successful usable
text reads use `read_status: live`; failed or unsupported reads retain the indexed snippet
with `read_status: indexed_only`. An indexed-only source is never silently
presented as a fresh page read. PDF and other recognized binary responses are
not parsed as fresh page text; their indexed excerpts may still be returned.

`currency` remains `unverified`. `indexed_at` converts the provider's index
metadata timestamp to UTC; `fetched_at` records a successful current page read;
`retrieved_at` records this bundle's retrieval. None is a law-effective date,
proof of current law, or proof that every later model claim is supported. A live
fetch can retrieve a page that describes older law.

The index worker has a 55-second budget. Each source read gets at most eight
seconds, limited by the remaining budget, inside the tool's 60-second caller
budget. Missing credentials make the service
unavailable. No usable official sources, cap exhaustion or provider failure
produces a static failure with no bundle. There is no automatic retry, browser
fallback, alternate provider or provider-terms acceptance. A failed page read
within a successful bundle is explicitly marked `indexed_only`.

## Tool contract

`america_ask` requires exactly these arguments:

```json
{"question": "Where can I read the official United States Code?", "jurisdiction": "U.S. federal"}
```

Question is 1–4,000 characters; jurisdiction is 1–100 or `null` for clarification.
Blank questions are invalid; blank jurisdiction requests clarification.
A successful Alexandria bundle has `ok: true`, `status: sources_found`,
`provider: firecrawl_gov_index`, the question/jurisdiction, UTC `retrieved_at`,
`legal_advice: false` and one to three source records:

```json
{
  "title": "United States Code",
  "url": "https://uscode.house.gov/",
  "currency": "unverified",
  "excerpt": "Indexed source excerpt.",
  "page_text": "",
  "read_status": "indexed_only",
  "text_truncated": false,
  "indexed_at": null,
  "fetched_at": null
}
```

This is a shape example, not a recorded lookup. There is no `answer` field:
Eve's normal chat/call response synthesizes and cites these sources.

| Status | Result |
|---|---|
| `sources_found` | Alexandria: source bundle, not an America.gov answer. |
| `answered` | Explicit legacy browser/API: complete America.gov Markdown answer and official search groundings. |
| `needs_jurisdiction` | Reason and legacy chat link; no external question. |
| `invalid_request` | Reason and legacy chat link; no external question. |
| `unavailable` | Disabled, caller denied or Firecrawl not configured; legacy browser also uses this for missing Chrome/browser extra. |
| `upstream_unavailable` | Index, transport, timeout, unsupported output or legacy completion failure. Static reason and legacy chat link; no answer or sources. |

The retained `america_url` failure field links to optional legacy chat; returning
that link does not submit a fallback question.

## Explicit legacy America.gov transports

`EVE_AMERICA_TRANSPORT=browser` uses normal Chrome chat controls; `api`
selects the observed direct HTTP adapter. No supported public America.gov
integration API was verified. On October 4, 2026, an owner's ordinary browser
question returned HTTP 200 with SSE, direct HTTP returned 418, and Eve's
own-Chrome probe met Cloudflare security verification before Send. Reliable
unattended access and a measured speedup were not established. No challenge is
solved or bypassed. Alexandria does not call America.gov chat.

Browser mode needs the existing `browser` extra and either
`EVE_BROWSER_OWN=1` or ordinary Chrome attach configuration. It retains a
dedicated connection/tab and semantic recipes, creates a fresh document per
question and verifies one exact fill/submit. `EVE_AMERICA_JEV=1` enables
recovery with an existing Jev route/key; known controls need no Jev call. Recovery
receives labels/roles and the chat URL, without field values or legal prose.
The route guard rejects extra history/metadata, duplicate attempts and recent
reused IDs before egress. Cancellation quarantines the worker until it stops;
shutdown closes only its own tab/connection.

Both legacy modes require supported SSE headers, ordered text, final
`answerComplete` metadata and terminal `[DONE]`. Parsing accepts at most 1 MiB
and 24,000 answer characters; this is not a Chrome download-buffer limit. Direct
API mode has finite reads, no redirects and a fresh client per call. Neither
repeats an ambiguous submission, replays signed history or uploads files.
The [legacy OpenAPI](contracts/america-gov.openapi.json) preserves those observed
requests/events and normalized results.

## Data and provider terms

Default mode sends the explicit question and jurisdiction to Firecrawl's
government index. Source hosts receive ordinary reads, without the Firecrawl
credential or conversation context. Retrieved text passes through the existing
shared chat/call brain and normal Eve conversation handling. The adapter has
no independent question journal. Do not include personal identifiers. Provider
terms, caching and retention still apply; this tool does not accept provider
terms for the owner.

Only explicit legacy mode sends the question to America.gov. Its
[terms](https://america.gov/terms) describe an information service rather than
professional advice or a filing channel, and prohibit automated scraping. Its
[privacy policy](https://america.gov/privacy-policy) describes caching and
measurement/security retention. Fresh browser state does not establish server
deletion or retention guarantees.

## Verification

Saved catalog evidence identifies the capability and options. An earlier direct
capability lookup returned three session-URL variants of the same official U.S.
Code page, reporting two credits. That evidence motivates canonicalization.

On October 4, 2026, the actual default adapter completed a generic U.S. Code
source lookup with `sources_found` and three unique official URLs. The shared
Firecrawl call balance decreased by one; the receipt reported two credits,
recorded as unpriced because no credit price was configured. The Library of
Congress guide supplied 6,000 characters of live page text, marked truncated;
two Congress.gov sources
retained indexed excerpts with `read_status: indexed_only`. All three remained
`currency: unverified`. This verifies library retrieval, not current law or a
completed installed-daemon conversation.

The authorized daemon restart was verified on October 4, 2026: it loaded this
editable checkout with America enabled in `alexandria` mode, answered IPC
`get_state`, and exposed `/america` with the expected owner-menu description.
Acceptance gates passed 260 offline tests and lint. Menu verification used a
read-only Telegram query; no unsolicited test message was sent. This verifies
the library, software runtime and menu, rather than an end-to-end live chat
answer or hardware health.
