# WebMCP probe

[WebMCP](https://developer.chrome.com/docs/ai/webmcp) lets a website register its own
tools on `document.modelContext`, each with a name, a description and an input schema.
An agent can call such a tool instead of clicking through the page. One call can replace
a dozen grounded clicks, if the sites eve visits offer tools at all.

The probe measures that. It decides nothing.

## What it does

When the browser lane reads a new page (http or https, a new host or path), it runs
one script: is `document.modelContext` there, and if so, what does `getTools()` list?
`getTools()` gets 300 ms. A probe that fails or times out is silent and never costs
the step. It runs once per page, so a second read of the same page, or a new query
string, does not run it again.

It writes one line to `~/Library/Logs/eve-bridge.log`:

```
browser lane: webmcp probe host=shop.example api=True tools=2 names=add_to_cart,checkout
browser lane: webmcp probe host=news.example api=False tools=- names=-
```

The line holds the host and the tool names, never the path or the query. A tool name
keeps only `A-Z a-z 0-9 _ . -`, cut to 64 characters, at most 50 names. Nothing
leaves the Mac. The lane never calls a tool.

## Reading it

```
grep 'webmcp probe' ~/Library/Logs/eve-bridge.log | grep -c .
grep 'webmcp probe' ~/Library/Logs/eve-bridge.log | grep -c 'api=True'
grep 'webmcp probe' ~/Library/Logs/eve-bridge.log | grep -v 'tools=-' | grep -v 'tools=0'
```

The first count is pages probed, the second is pages with the API, and the third lists
pages that registered tools. If a real share of the pages eve uses register tools, the
next step is a step that calls the site's tool before it clicks. That step would be
off by default and judged on a holdout set like the other lanes. If almost none do,
remove the probe.

## Not covered

- Only the browser lane (`browser_lane.py`, Steel or Chrome) runs the probe. Codex's
  own browser does not. A WebMCP call from Codex still asks first
  (`test_codex_permissions.py`).
- Chromium without WebMCP enabled has no `document.modelContext`, so every page
  there reads `api=False`, whatever the site offers.

## Tests

`bridge/tests/test_webmcp_probe.py`: the probe is off for a bare `_Page`, runs once
per page, logs only the host and cleaned names, skips non-web pages, and is silent
on failure. The live test (`EVE_LIVE=1`) loads two pages in real Chromium. The one
with a shimmed `getTools()` logs both tools; the plain one logs `api=False`.
