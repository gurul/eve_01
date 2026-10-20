"""eve-web's curl_cffi helper for watch::tls: the one exchange of each hop of tls_request, as watch.py makes it.

Run as ``python3 -I -c <this file's text>``. The first stdout line is ``{"ready": true}`` once curl_cffi is
imported, or ``{"error": "missing"}`` when it is not installed. Then each stdin line is one hop, as JSON: ``url``,
``resolve`` (the pinned ``host:port:ip``), ``left`` (seconds until the whole read's deadline), ``max_bytes`` and
``impersonate``; each answer is one stdout line, as JSON: ``status``, ``headers`` (name/value pairs),
``primary_ip`` and ``body`` (base64), or ``error``: "too_big", "timeout" or "unreachable". Everything else (the
URL check, the pin, redirects, the check of the address libcurl used) stays in Rust.
"""

import base64
import json
import sys
import time


def answer(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


class _TooBig(Exception):
    pass


class _TooLong(Exception):
    pass


def hop(cffi, CurlOpt, req, deadline):
    max_bytes = int(req["max_bytes"])
    opts = {CurlOpt.RESOLVE: [str(req["resolve"]).encode()], CurlOpt.NOPROXY: b"*",
            CurlOpt.PROTOCOLS_STR: b"http,https", CurlOpt.REDIR_PROTOCOLS_STR: b"http,https",
            CurlOpt.MAXFILESIZE: max_bytes}
    left = deadline - time.monotonic()
    if left <= 0:
        return {"error": "timeout"}
    chunks = []
    try:
        with cffi.Session(impersonate=str(req["impersonate"]), trust_env=False, curl_options=opts) as s:
            r = s.get(str(req["url"]), allow_redirects=False, timeout=left, stream=True, proxy=None,
                      headers={"Accept-Language": "en-US,en;q=0.9"})
            try:
                size = 0
                for chunk in r.iter_content():
                    size += len(chunk)
                    if size > max_bytes:
                        raise _TooBig()
                    if time.monotonic() > deadline:
                        raise _TooLong()
                    chunks.append(chunk)
                status, headers, used = r.status_code, r.headers, str(getattr(r, "primary_ip", "") or "")
            finally:
                r.close()
    except _TooBig:
        return {"error": "too_big"}
    except _TooLong:
        return {"error": "timeout"}
    except cffi.exceptions.Timeout:
        return {"error": "timeout"}
    except Exception as e:  # noqa: BLE001 — libcurl's errors (refused, reset, too big) are the site's, never ours
        if "curl: (63)" in str(e):               # CURLE_FILESIZE_EXCEEDED: a Content-Length past MAXFILESIZE
            return {"error": "too_big"}
        if time.monotonic() >= deadline:
            return {"error": "timeout"}
        return {"error": "unreachable"}
    multi = getattr(headers, "multi_items", None)
    pairs = list(multi()) if callable(multi) else list(headers.items())
    return {"status": int(status), "headers": [[str(k), str(v)] for k, v in pairs], "primary_ip": used,
            "body": base64.b64encode(b"".join(chunks)).decode("ascii")}


def main():
    try:
        from curl_cffi import CurlOpt
        from curl_cffi import requests as cffi
    except ImportError:
        answer({"error": "missing"})
        return 0
    answer({"ready": True})
    for line in sys.stdin:
        if not line.strip():
            continue
        t0 = time.monotonic()
        req = json.loads(line)
        answer(hop(cffi, CurlOpt, req, t0 + float(req["left"])))
    return 0


sys.exit(main())
