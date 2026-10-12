# Ask science: searching research papers

Owner, 2026-10-12: "allow me to query on eve as well". eve can search about 200 million scholarly papers (OpenAlex
titles and abstracts) with one tool, `science_search`, in the chat, the app chat and out loud
(`bridge/src/eve_bridge/science_search.py`).

The search is ScienceMadeReadable's public API, `https://api.sciencemadereadable.com/api/data`. It is free, needs
no key and no account, and is read-only. Its guide is `<base>/llms.txt` and its parameters are `<base>/openapi.json`.

## When eve uses it

The tool's description tells the model to use it when the owner asks:

- what the research, the studies or the science say about something ("is intermittent fasting better than
  calorie counting?", "what does the research say about screen time and toddlers' sleep");
- for papers or evidence on a question, or a review of a topic;
- about a specific study, paper or DOI.

It is not for news, prices, how-to steps, everyday facts or anything else on the general web: those stay with
`web_search`. While both are offered and the tool-choice lines ride (`EVE_MENU_TOOLS`), one line in the
instructions says the same.

The description also tells the model how to answer: from the results only, citing each paper it relies on by its
title and year with its link (out loud, the title or first author and the year), saying that only abstracts were
read, and saying plainly when the evidence is thin, mixed or old, or when nothing relevant came back.

## What it takes and returns

| Argument | Meaning |
|---|---|
| `query` | What to find. A plain question searches by meaning; quotes, `AND`, `OR`, `NOT`, brackets and `title:` search by exact words (the service picks from the query's own syntax). Control characters are removed and it is clipped to 500 characters; under 2 characters is refused before any request. |
| `k` | How many papers, 1 to 20 (clamped); null for 8. |
| `year_from`, `year_to` | Publication years, or null. Clamped to 1000 through next year, and swapped when given in the wrong order. |
| `reviews_only` | true for review articles only (`type=review`). |
| `open_access_only` | true for papers anyone can read for free (`open_access=true`). |

The tool is strict: every argument is required, and all but `query` may be null.

The answer is `{"ok": true, "query", "searched", "count", "results", "note"}`. `searched` is "by meaning" or "by
exact words" (with `matching_papers`, the service's exact count), or says the exact words matched nothing and the
service searched by meaning instead. Each paper is compact, sized for a model's context (about 1,100 characters,
so about 9,000 for the default 8):

```json
{"title": "...", "year": 2024, "type": "review", "venue": "...", "authors": "A, B, C and 5 more",
 "citations": 45, "open_access": true, "doi": "10.xxxx/...", "link": "https://doi.org/10.xxxx/...",
 "page": "https://sciencemadereadable.com/?work=10.xxxx/...", "abstract": "the first ~600 characters…"}
```

- `authors` is at most the first three names, then "and N more" from the paper's full count.
- `link` is the paper's own page (its DOI or PubMed address), else its DOI, else its OpenAlex page.
- `page` is the paper on sciencemadereadable.com's map of science, with its citations. `plain_words`, present only
  when the site can open the paper, is the site's rewrite of it for a lay reader.
- `abstract` has the publishers' markup tags removed and is clipped to 600 characters.
- Scores, junk ratings, subject labels, keywords and PDF links are left out.

A failure is `{"ok": false, "reason"}` in words the model can repeat.

## Failures and limits

- **Timeouts and 5xx.** One request has 20 seconds. A timeout, a dropped connection or a 5xx (the service is
  briefly down during its nightly data update) is retried once, a second later, within 25 seconds for the whole
  call.
- **429.** The public service allows 30 requests a minute per address. A 429 is never retried: the reason says how
  long to wait (from `Retry-After`, or the body's `retry_after`) and tells the model not to search again in that
  turn; `retry_after_s` carries the number.
- **eve's own ceiling.** Before any request, a retry included, the bridge keeps itself under 20 requests in any 60
  seconds, so a loop cannot spend the address's allowance. Over it, a search gets the same busy reason and nothing
  is sent, and a retry is skipped (the first failure is the answer).
- **400, 404, 422.** Not retried; the reason carries the service's own detail.
- **Coverage.** Titles and abstracts only, in English, from an OpenAlex snapshot: no full texts, and very recent
  papers arrive a few days late. Retracted papers are left out by the service.

## Outside text

Titles and abstracts are other people's words. The taint gate (`tool_gate.py`) treats `science_search` as a read
that is neither acting nor clean, the same as `web_search`: it never asks for confirmation, and its result taints
the turn, so an acting call later in that turn follows the confirmation rules for outside text (the question shows
the whole act). In the owner's question the source reads "after reading a search of research papers". Its results
are kept in the transcript like `web_search`'s (`telegram.TRANSCRIBED_TOOLS`, `voice_agent.ANSWER_TOOLS`), so "the
second paper" resolves later, and they are marked there as outside words.

## Where it is offered

- The chat brain (`telegram.py`), which also serves the Eve Link app chat: every turn while the switch is on,
  beside the phone's location and the camera's tools. The toolset router does not route it.
- The voice backend (`voice_agent.py`): every conversation while the switch is on, never in a lesson (think out
  loud). The call runs as a slow tool, so the conversation keeps flowing while it searches.
- A rundown does not offer it.
- The intent eval (`bridge/tools/intent_eval.py`) labels it `science_search`.

## Privacy

- The query and the filters go to the service, which keeps them in its request log (its guide says the log does
  not record the address). Nothing else of the owner's is sent: no key, no account, no history.
- eve's log never has the query: a line says how many results came back and how long it took. httpx logs each
  request's URL, so a filter on the `httpx` logger shows `q=<hidden>` in this service's URLs
  (`science_search.QueryFilter`).
- It costs nothing, so the spending ledger has no line for it.

## Turning it off

`EVE_SCIENCE_SEARCH` in `~/.config/eve-bridge/env` is on unless it is `0`, `false`, `no` or `off`. Off, the tool
is not offered anywhere, and a stray call gets a reason without a request.

## Tests

`bridge/tests/test_science_search.py` covers the schema and its words, argument validation and clamping, the
answer's shape and clipping, 429, 5xx, timeouts, refusals and bad JSON over a mocked transport, the bridge's own
ceiling, the log filter, the switch, the taint gate, the transcript, and each surface (chat, app chat, voice, the
tool-choice line, the intent eval's label). The suite runs with the switch off (`conftest.py`), because its
request-body checks were written without the tool; these tests switch it on. One live test, skipped unless
`EVE_LIVE=1`, runs one real search.

The tool is a prompt change, and its tool-choice line is new: a blind intent eval (rule 6 in `AGENTS.md`) has not
been run on it yet.
