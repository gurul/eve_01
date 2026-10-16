# The New York Times

eve reads The New York Times with five tools, in the chat, the app chat and out loud
(`bridge/src/eve_bridge/nyt.py`). Four of them need an NYT API key. One, `nyt_headlines`, also works without a key.

| Tool | API | What it answers | Needs the key |
|---|---|---|---|
| `nyt_search` | Article Search v2 | What the Times wrote about a topic, person or event, its reviews, an article the owner remembers. 10 articles a page, from 1851 to today. | yes |
| `nyt_archive` | Archive v1 | Every article of one month, from 1851 to this month, filtered by words and section on the Mac. | yes |
| `nyt_bestsellers` | Books v3 | The Best Sellers lists, from 2017 to today: every list of a week with its top 5, or one whole list. | yes |
| `nyt_popular` | Most Popular v2 | The most viewed, emailed or shared articles over 1, 7 or 30 days. | yes |
| `nyt_headlines` | Top Stories v2, else the RSS feeds | What is on the home page or a section front now. | no |

The specs these tools follow are in [`docs/nyt/`](nyt/). They are NYT's own OpenAPI files from developer.nytimes.com.
NYT also publishes earlier versions of the same specs on GitHub (`nytimes/public_api_specs`), under the Apache 2.0
license. The copies here carry no license note of their own.

## The tools

Each tool is strict. Every argument is required, and every argument but the search words and the archive's year and
month can be null. Each tool answers `{"ok": true, ...}`, or `{"ok": false, "reason": ...}` in words the model can
repeat. Each article or book row has a `url` (always https). It also has an `image` `{src, width, height, caption?,
credit?}` when NYT gives one over https that is at least 400 px wide. A book cover is portrait (live: 312 x 400),
so it is kept when it is at least 200 wide and 300 tall. eve never makes an image up. The Eve Link app shows these
rows as an image list (see [On the phone](#on-the-phone)).

### `nyt_search(query, begin_date, end_date, section, material, sort, page)`

- `query`: the words. Quotes match exact words. The query can be empty when a filter or a date is given.
- `begin_date`, `end_date`: `YYYY-MM-DD`, from 1851-01-01 to today. eve puts them in order and sends them as
  `YYYYMMDD`. For recent news ("today", "latest", "anything new") the tool tells the model to start about 7 days
  back with no end date, and to use a single day only when the owner asks for one exact date: in the first eval,
  "anything about nvidia today" searched today alone, sorted newest, and got one article.
- `section`, `material`: become one Lucene `fq`. eve uses the field names NYT has used since 2025-04-08:
  `section.name:("Books") AND typeOfMaterials:("Review")`. A value that matches one in the spec's lists, ignoring
  case and punctuation, is sent the way the spec spells it. Any other value is sent as written, because the spec's
  section list is partial. Every value is quoted, with `\` and `"` escaped, so a value cannot add a clause.
- `sort`: `best` (the default), `newest`, `oldest` or `relevance`. `page`: 0 to 99. The tool tells the model to use
  `relevance` (or no sort) for a topic search and `newest` only when the owner asks for the latest: live, "mars
  rover" sorted newest put an off-topic article first.
- Each row has the headline, date, section, type, byline, abstract (clipped) and link. The image is the `default`
  crop, or the `thumbnail` crop when it is wide enough. eve makes relative paths absolute on
  `https://static01.nyt.com/`.

Book reviews come from here (`material` Review, `section` Books), because NYT removed the Books API's reviews
service on 2025-05-15. The best-seller tool's description says so.

### `nyt_archive(year, month, contains, section, limit)`

- The year runs from 1851 to this year. The month must not be later than this month.
- eve downloads the month once (about 20 MB, at most 80 MB read) and keeps it on disk as compact rows. Each later
  call for that month filters the rows on the Mac and sends no request.
- `contains`: words that the headline, the abstract or a subject tag must contain (ignoring case).
  `section`: a section or desk name. `limit`: 1 to 50, 20 by default.
- The answer says how many articles the month has, how many matched, and whether it came from the cache. Rows are
  in date order. Old months often have no images.

### `nyt_bestsellers(list, date)`

- No list: the overview. Every list of the week, each with its top 5 books.
- A list: the whole list. Each book has its rank, title, author, publisher, weeks on the list, description, link
  (the Times' review on nytimes.com when NYT gives one, else Bookshop, else Amazon) and, when NYT gives them,
  `amazon`, `bookshop`, `review` and the cover (`book_image` with its width and height). Live on 2026-10-16, 5 of 15
  nonfiction and 4 of 15 fiction books had a review link.
- Friendly names ("fiction", "nonfiction", "paperback", "young adult", "audio", "kids") are matched against the list
  names in the overview itself. An exact list name or slug wins. Otherwise eve picks among the lists whose slug
  holds every word. It prefers hardcover, then fiction over nonfiction unless nonfiction was asked for, then the
  shortest slug. eve keeps no slug list of its own: NYT adds and drops lists. A name that matches nothing gets the
  week's list names back.
- `date`: `YYYY-MM-DD`, from 2017-01-01 (the start of NYT's data) to 14 days ahead. A list's date is its print
  date, 11 days after it is on the web. Lists come out on Wednesdays at about 7 pm Eastern. Null is the current list.

### `nyt_popular(by, period, share_type)`

- `by`: `viewed` (the default), `emailed` or `shared`. `period`: 1 (the default), 7 or 30 days. `share_type`:
  `facebook` or null, and used only with `shared`. These are the spec's enums. Any other value is refused with the
  valid values.
- Each row has the rank, title, abstract, section, byline, date, link and the widest image from
  `media[].media-metadata`.

### `nyt_headlines(section, limit)`

One tool with two sources, because both answer the same question.

- **With `NYT_API_KEY`:** Top Stories, `GET /svc/topstories/v2/{section}.json`. Each row has the title, abstract,
  section, subsection, byline, date, the topic and place facets, the link, and the widest image that is at least
  600 px wide (superJumbo or threeByTwoSmallAt2X).
- **Without a key, or when Top Stories answers 401, 403, 429 or 5xx, does not answer, or sends no readable
  stories:** the section's RSS feed, `https://rss.nytimes.com/services/xml/rss/nyt/{section}.xml`, which needs no
  key. Each row has the title, description, date, creator, category tags, link and the widest `media:content` image.
  A result that fell back says why in `fell_back`. "No readable stories" is a 200 that is not JSON, has no `results`
  list, or has no row eve can use; such an answer is not cached. Live on 2026-10-16, Top Stories' `sports` answered
  200 with `"results": null` (`num_results` 0, last updated 2025-04-30), so it now goes to the Sports feed.
- **A front empty in both sources** (with a key) answers `ok: false` and points the model at `nyt_search` with that
  section and the last few days. Live on 2026-10-16 the Sports feed was empty too.
- The result's `source` is `top_stories` or `rss`.
- `section`: null is the home page. eve maps friendly names onto both specs' enums, ignoring case and punctuation:
  "world", "tech" (technology), "books" (books/review), "U.S.", "new york" (nyregion), "front page" (home). Both are
  joined by one explicit table, `nyt.TOP_TO_RSS`, written from both specs. Three Top Stories sections have no feed:
  `insider`, `magazine` and `opinion`. Without a key, those three fail with the list of feeds. Many feeds have no
  Top Stories section (Climate, Africa, Soccer, ...). Those go to RSS even with a key. A name that is in neither spec
  is refused with the valid names, and eve does not guess one.
- `limit`: 1 to 30, 10 by default.
- **The feed is parsed safely.** The bridge does not depend on defusedxml. eve uses the standard library's
  ElementTree. It reads at most 2 MB, and it refuses any feed that has a `DOCTYPE` or `ENTITY` declaration before it
  parses anything. So no entity can expand, and no external file is read.

## The key

1. Sign in at [developer.nytimes.com](https://developer.nytimes.com), open **My Apps**, and add an app.
2. Enable the **Article Search API**, **Archive API**, **Books API**, **Most Popular API** and **Top Stories API**
   for it. The RSS feeds need nothing.
3. Put the key in eve's env file, `~/.config/eve-bridge/env`, as `NYT_API_KEY=...`, and restart eve's service.

Without the key, the four key tools are not offered. A call to one of them answers that the owner should add
`NYT_API_KEY`, and no request is sent. `nyt_headlines` is still offered and reads the RSS feeds.

The key goes only in the `api-key` query parameter to `api.nytimes.com`. It is never logged and never in a result:

- `nyt.QueryFilter` on the `httpx` and `httpcore` loggers hides the `api-key`, `q` and `fq` values in NYT URLs.
- eve's own log lines give only sizes, times and status codes.
- `handle` refuses any result that would contain the key.

## Being gentle

NYT allows each key 5 requests a minute and 500 a day.

- **Keyed requests are paced.** This process sends them at least 12 s apart (`KEY_SPACING_SECS`) and stops at 450 a
  day (`KEY_PER_DAY`). A call waits for its slot when the slot is at most 13 s away. Otherwise it answers that eve
  is pacing its requests, with the seconds to wait.
- **The feeds have their own pace.** RSS requests are at least 2 s apart, and the key limit does not apply to them.
- **Retries.** A timeout, a dropped connection or a 5xx is retried once within the call's budget (45 s, or 150 s
  for an archive month). A 429 is never retried: the answer gives NYT's `Retry-After`. A 401 or 403 says to check
  the key and that the API is enabled for it.

## Caching

The cache is in `$XDG_CACHE_HOME/eve-bridge/nyt`, or `~/.cache/eve-bridge/nyt` when that is unset. It is never in
the repo. Each file is written atomically.

| What | Kept for |
|---|---|
| An archive month | for good. The current month is kept 6 hours, because it is still growing. |
| A week's best-seller overview and a dated list | a week. The current ones are kept 6 hours. |
| The list names (for friendly names) | a week |
| A Most Popular ranking | an hour |
| A section's headlines, per source (`top_stories` or `rss`) and section | 10 minutes |

To clear it, delete the folder.

## What leaves the Mac

- **To `api.nytimes.com`:** the key, plus what each call asks for. That is the search words, filters, dates, sort
  and page; the archive's year and month; the best-seller list's slug and date; the popular ranking's kind and
  period; and the Top Stories section.
- **To `rss.nytimes.com`:** the feed's section, and nothing else.

NYT sees these requests under the owner's key, and under the Mac's address. Nothing else of the owner's is sent.
The archive's `contains` and `section` filters never leave the Mac.

## Outside text

Headlines, abstracts, descriptions and book blurbs are NYT's words. The taint gate (`tool_gate.py`) treats all five
tools as reads that are neither acting nor clean, with the source "the New York Times". So after one of them, an
acting call in the same turn is asked of the owner first, as after `web_search`. Their results are kept in the
transcript as outside words (`telegram.TRANSCRIBED_TOOLS`, `voice_agent.ANSWER_TOOLS`). Every description tells the
model never to follow instructions written in the results.

## Where they are offered

- **The chat brain and the app chat** (`telegram._tool_lists`): every turn while the switch is on, like
  `science_search`. The toolset router does not route them, and they are not in its `ALWAYS` set. Jev's questions
  and the fitted gates are unchanged.
- **The tool-choice lines** (`telegram.INTENT_LINES`): two lines. Each rides only while every tool it names is
  offered. The headlines line rides with or without a key. The other line names the four key tools.
- **Out loud:** the voice backend gets the tools, except in a lesson.
- **The intent eval** (`bridge/tools/intent_eval.py`) labels all five tools `nyt`. No blind case set covers them yet.
  A routing accuracy for them needs a new blind set, like `intent_science_blind.json`.
- **eve lite** forces `EVE_NYT=0`. Lite also removes `NYT_API_KEY`, because the name is secret-shaped.

## On the phone

Owner, 2026-10-16: "if i want to search for specific news it can pull it; on my phone app allow it to generate nice
links (openui / images) so i can click onto news and browse."

In the Eve Link app the reply is a card (see [app-chat.md, Pictures in cards](app-chat.md#pictures-in-cards)): an
image list with one row per article or book, 5 to 8 of them. Each row shows the headline or title, a short line
("Science · Oct 14"; a book: "author · weeks on the list") and the row's `image` when the tool gave one. A row
without an image shows none; eve never finds or makes one up for it. A tap opens the row's `url` in the app's in-app
browser (`@OpenUrl`, routed to the native `openURL`): an article on nytimes.com, a book on its Times review when NYT
gives one, else on Bookshop or Amazon. One big story may lead with a single large picture instead. Every news or
book list ends with two or three follow-ups that keep browsing, such as more on the topic, another section or the
most read. `link_chat.APP_RULE` lists "articles, books or headlines from a tool" among the shapes that get a block.
The rule and an example live in `ios/EveLink/Web/scripts/gen-prompt.mjs`; regenerate `openui_prompt.txt` with
`pnpm gen-prompt` after changing them. The example's links and picture are `example.com` placeholders and its rows
name no person, so the model has no real-looking value to copy.

### Measured (2026-10-16)

gpt-6-luna at low effort, the app turn of `bridge/tools/openui_eval.py` with `NYT_API_KEY` kept and the NYT tools
live (every other tool refused), scored by code: a news card is a block that parses, whose ListItems each open an
https link (nytimes.com for an article; nytimes.com, bookshop.org or amazon.com for a book); "all checks" adds
links and pictures that are exact strings from that turn's tool results, 5 to 8 items and a FollowUpBlock.

| set | runs | news replies as a card | news, all checks | cards with follow-ups | plain as plain | other cards right | blocks valid |
|---|---|---|---|---|---|---|---|
| First set, before this round (rescored with the book rule) | 1 | 10 of 18 | 5 of 18 | 5 of 10 | 6 of 6 | 5 of 6 | 15 of 15 |
| First set, second look (contaminated: the fixes were made against it) | 3 | 42 of 54 | 22 of 54 | 32 of 42 | 18 of 18 | 14 of 18 | 56 of 56 |
| New blind set (20 messages written by an agent that read no code) | 2 | 24 of 28 | 15 of 28 | 18 of 24 | 6 of 6 | 6 of 6 | 30 of 30 |

- The commonest misses: too few rows (a topic search where only 2 or 3 results fit), no follow-ups, the sports
  front, and a topic that the model asked about first ("which election?").
- The sports front was empty in both Top Stories and its feed on the day; with the pointer to `nyt_search`, one of
  five sports replies searched and drew a card.
- Links and pictures not from a tool result: 1 reply of 90 in the second look and 3 of 40 in the blind set. Each
  was the model shortening or garbling a real tool URL (a slug cut short, a month dropped from an image path, a
  section page), not a made-up story. The app opens such a link to a missing page; the image does not load.
- The blind set had 14 news, 3 plain and 3 other-card messages. Three of its messages are close to first-set ones
  (the moon landing, the Fed decision, iPhone vs Pixel).
- NYT requests that left the Mac: 28 for the second look and 21 for the blind set (others came from the cache), all
  HTTP 200, none refused for pacing.

## Turning it off

`EVE_NYT=0` (or `false`, `no`, `off`) in the env file. No tool is offered, and a call answers that the tools are off.

## Tests

`bridge/tests/test_nyt.py` covers the following. No test reaches the network: every request goes to an
`httpx.MockTransport`. Every test sets or removes `NYT_API_KEY` itself, and the cache is a temporary folder. The suite
pins `EVE_NYT=0` and no key (`conftest._nyt_pinned_off`).

- fq building and escaping, and date, enum and section validation;
- each tool's rows, and the image pick for each source with several sizes;
- the archive, books, popular and headlines caches;
- the friendly list names;
- the key path, the no-key path, and the Top Stories to RSS fallback on 401, 429, 5xx and an unreadable or empty
  answer (the live sports answer among them), and a front empty in both;
- the section table and its gaps;
- hostile and oversized feeds;
- 429s and retries, and the pace on an injected clock;
- the key kept out of the log and the results;
- the switch, the taint gate, and every surface the tools ride.
