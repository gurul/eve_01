"""Signs a Telegram Mini App initData string, the way Telegram does, for the tests.

Not a test module (no test_ prefix). The bridge only verifies initData (miniapp.check_init_data); Telegram signs it.
"""

from __future__ import annotations

import hashlib
import hmac
from urllib.parse import urlencode


def sign_init_data(fields: dict[str, str], token: str) -> str:
    """Build a signed initData string (what Telegram hands the page)."""
    check = "\n".join(f"{k}={fields[k]}" for k in sorted(fields))
    secret = hmac.new(b"WebAppData", token.encode(), hashlib.sha256).digest()
    return urlencode({**fields, "hash": hmac.new(secret, check.encode(), hashlib.sha256).hexdigest()})
