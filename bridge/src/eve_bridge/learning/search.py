"""Optional practice references through TinyFish search; learner work and images are never sent.

Only the topic and level leave the Mac. One TinyFish search and one fetch of its top three results
(web_reader.search_and_read, through the watcher's guarded http_request) give the pages as markdown; the tutor
uses them as inspiration for an original, adapted problem. Exa did this until 2026-09-26, with its content
moderation on; TinyFish's search has no such option (docs.tinyfish.ai, 2026-09-29), so the tutor prompt, which
adapts rather than copies, is the filter now.
"""
from urllib.parse import urlsplit

from .. import spend, tinyfish, watch

RESULTS = 3
TEXT_CHARS = 3000


def search_problems(topic, level):
    if not tinyfish.api_key():
        return [], ("TinyFish search is off. Add TINYFISH_API_KEY to ~/.config/eve-bridge/env and restart "
                    "to enable practice references.")
    from ..web_reader import search_and_read

    query = f"Practice problems and exercises: {str(topic)[:200]}. Level: {str(level)[:100]}"
    try:
        found = search_and_read(query, limit=RESULTS)
    except watch.FetchError as exc:
        why = f" (HTTP {exc.status})" if exc.status else ""
        return [], f"TinyFish search failed{why}. Eve created a problem without search references."
    except (OSError, ValueError, TypeError):
        return [], "TinyFish search was unavailable. Eve created a problem without search references."
    pages = found.get("pages") or []
    spend.record("tinyfish", "search+fetch", spend.LESSONS, 0.0, note=f"{len(pages)} pages, free")
    sources = []
    for page in pages:
        url, content = page.get("url"), page.get("markdown")
        if not isinstance(url, str) or not isinstance(content, str) or not content.strip():
            continue
        parsed = urlsplit(url)
        if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
            continue
        sources.append({"title": str(page.get("title") or parsed.hostname)[:200],
                        "url": url[:2000], "text": content[:TEXT_CHARS]})
    return sources, ("Practice references found with TinyFish; Eve creates an adapted problem."
                     if sources else "TinyFish found no usable references. Eve created a problem without "
                                     "search references.")
