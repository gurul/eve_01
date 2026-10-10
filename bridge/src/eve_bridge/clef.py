"""Cloudflare's Clef behind the visual judge's fallback slot: Jev's wire format, plus images.

What Clef is (scouted 2026-10-10 from Cloudflare's blog, model page and changelog of 2026-10-01): a family of
"System One" decision models, Clef (27B) and Clef-flash (9B), on Workers AI, described as fully Jev-API compatible:
a state and typed questions (noul, choice, score) in, calibrated probabilities out. Unlike Jev it has a vision
encoder (at most 4 images a request) and a 64k context. It is not on OpenRouter; it needs a Cloudflare account id
and an API token with the Workers AI permission:

    POST https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID/ai/run/@cf/cloudflare/clef
    Authorization: Bearer $CLOUDFLARE_AUTH_TOKEN
    {"model": "clef" | "clef-flash", "state": ..., "questions": {...}, "images": ["data:image/png;base64,..."]}

The model page gives the reply as ``{model, answers, usage}``; Cloudflare's REST API wraps replies in
``{"result": ..., "success": ..., "errors": [...]}``. Both shapes are accepted here (``result`` unwrapped, then
``jev.normalize``), and ``success: false`` is an error. The path (``EVE_CLEF_PATH``) and the body's model
(``EVE_CLEF_MODEL``) come from config, because a separate ``@cf/cloudflare/clef-flash`` path is only in secondary
sources. NOT YET CALLED: there is no Cloudflare key on this Mac, so this module is tested against mocked replies
only, and ``tools/visual_judge_eval.py --clef-probe`` checks the envelope with one live call before any Clef number
is recorded.

The slot is empty (``from_env`` returns None) unless both keys are present. Bodies are never logged.
"""

from __future__ import annotations

import base64
import io
import json
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any, Callable, Mapping, Optional

from . import jev, pricing, spend

ACCOUNT_ENV, TOKEN_ENV = "CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_AUTH_TOKEN"
PATH_ENV, MODEL_ENV, TIMEOUT_ENV = "EVE_CLEF_PATH", "EVE_CLEF_MODEL", "EVE_CLEF_TIMEOUT"
DEFAULT_PATH = "@cf/cloudflare/clef"
DEFAULT_MODEL = "clef"
MODELS = ("clef", "clef-flash")
DEFAULT_TIMEOUT_S = 2.0
BASE = "https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/{path}"
MAX_IMAGES = 4
MAX_IMAGE_BYTES = 4 * 1024 * 1024


class ClefError(RuntimeError):
    """One fixed line; never a body, a key or an account id."""


def route_config(env: Mapping[str, str]) -> Optional[tuple[str, str, str, float]]:
    """(url, token, model, timeout) or None when either Cloudflare key is missing."""
    account = str(env.get(ACCOUNT_ENV) or "").strip()
    token = str(env.get(TOKEN_ENV) or "").strip()
    if not account or not token:
        return None
    path = str(env.get(PATH_ENV) or DEFAULT_PATH).strip().lstrip("/")
    model = str(env.get(MODEL_ENV) or DEFAULT_MODEL).strip()
    if model not in MODELS:
        model = DEFAULT_MODEL
    try:
        timeout = float(str(env.get(TIMEOUT_ENV) or DEFAULT_TIMEOUT_S))
    except ValueError:
        timeout = DEFAULT_TIMEOUT_S
    if not 0 < timeout <= 10:
        timeout = DEFAULT_TIMEOUT_S
    return BASE.format(account=account, path=path), token, model, timeout


def image_data_url(image: Any) -> str:
    """A PIL image, or an existing data URL, as a PNG data URL of at most MAX_IMAGE_BYTES decoded. Remote URLs are
    refused (Clef rejects them too)."""
    if isinstance(image, str):
        if not image.startswith("data:image/"):
            raise ClefError("only data: image URLs are sent")
        raw = base64.b64decode(image.split(",", 1)[-1] or "", validate=False)
        if len(raw) > MAX_IMAGE_BYTES:
            raise ClefError("image over 4 MiB")
        return image
    copy = image.convert("RGB")
    for side in (2048, 1600, 1280, 1024, 768):
        copy.thumbnail((side, side))
        out = io.BytesIO()
        copy.save(out, format="PNG")
        if out.tell() <= MAX_IMAGE_BYTES:
            return "data:image/png;base64," + base64.b64encode(out.getvalue()).decode("ascii")
    raise ClefError("image over 4 MiB")


def unwrap(payload: Any, questions: Mapping[str, Any]) -> dict[str, Any]:
    """The Jev envelope from either reply shape: ``{"result": {...}}`` (REST) or the bare ``{answers}``."""
    if not isinstance(payload, dict):
        raise ClefError("reply is not an object")
    if payload.get("success") is False:
        raise ClefError("Cloudflare reported success false")
    inner = payload.get("result") if isinstance(payload.get("result"), dict) else payload
    body = jev.normalize(dict(inner), questions)
    if not isinstance(body.get("answers"), dict):
        raise ClefError("no answers in the reply")
    return body


def meter(model: str, body: Mapping[str, Any]) -> None:
    usage = body.get("usage") if isinstance(body.get("usage"), dict) else {}
    tokens = usage.get("input_tokens", usage.get("prompt_tokens"))
    tokens = tokens if isinstance(tokens, int) and not isinstance(tokens, bool) else None
    if tokens is None:
        spend.record("cloudflare", model, "computer_visual_judge", None, note="no usage in the reply")
    else:
        spend.record("cloudflare", model, "computer_visual_judge", pricing.estimate_clef_cost(model, tokens),
                     tokens={"in": tokens})


def make_predict(url: str, token: str, model: str, *, timeout_s: float = DEFAULT_TIMEOUT_S,
                 post: Optional[Callable[..., Any]] = None
                 ) -> Callable[[Any, dict[str, Any], list[Any], float], dict[str, Any]]:
    """``predict(state, questions, images, timeout) -> {"answers": ...}``. ``post`` takes urllib's
    ``(request, timeout=...)`` so tests run without a socket. One attempt: the judge's budget has no room for a
    retry after the failure that brought it here."""
    send = post or urllib.request.urlopen

    def predict(state: Any, questions: dict[str, Any], images: list[Any], timeout: float = timeout_s
                ) -> dict[str, Any]:
        if len(images) > MAX_IMAGES:
            raise ClefError("more than 4 images")
        body = {"model": model, "state": state, "questions": questions,
                "images": [image_data_url(im) for im in images]}
        request = urllib.request.Request(url, data=json.dumps(body).encode("utf-8"), method="POST",
                                         headers={"Authorization": f"Bearer {token}",
                                                  "Content-Type": "application/json"})
        t0 = time.perf_counter()
        try:
            with send(request, timeout=min(timeout_s, max(0.1, timeout))) as response:
                payload = json.loads(response.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            raise ClefError(f"HTTP {e.code}") from None
        except urllib.error.URLError:
            raise ClefError("unreachable") from None
        except (ValueError, TypeError) as e:
            raise ClefError(f"undecodable reply ({type(e).__name__})") from None
        except TimeoutError:
            raise ClefError("timeout") from None
        out = unwrap(payload, questions)
        meter(model, out)
        out.setdefault("ms", (time.perf_counter() - t0) * 1000.0)
        return out

    return predict


@dataclass
class ClefJudge:
    """The filled slot: ``predict(state, jev questions, images, timeout)``."""

    predict: Callable[[Any, dict[str, Any], list[Any], float], dict[str, Any]]
    model: str = DEFAULT_MODEL


def from_env(env: Mapping[str, str]) -> Optional[ClefJudge]:
    cfg = route_config(env)
    if cfg is None:
        return None
    url, token, model, timeout = cfg
    return ClefJudge(make_predict(url, token, model, timeout_s=timeout), model)
