"""eve's vault: sign-ins, cards and 2FA secrets that no model ever sees.

The owner, 2026-10-12, after comparing eve with OpenInstinct (Merit-Systems, MIT): its browser agent asks
for a vault item by an opaque handle and code fills the page, so a password never enters a model's context.
eve does the same. A model sees an item only as a line of this module's ``Item.line``: a handle, a kind, a
label and a site (``v_3k9f2a7q login "Chase" for chase.com``). It names the handle in a plan step
(browser_lane, plan_contract's ``fill``) or a tool call (vault_mcp, for Codex tasks), and this module hands
the values to the code that fills the field, never back to the model.

    eve-bridge vault add login --site chase.com --label Chase      values read at the terminal (getpass)
          │
          ▼
    macOS Keychain, service "eve-vault", account = handle          the values, as hex JSON
    ~/.config/eve-bridge/vault.json (0600)                         handle, kind, label, site: no value
          │
          ▼
    Vault.fill_values(handle, page_url)   a login or a 2FA code only on its own site; a card anywhere

Kinds: ``login`` (username, password), ``card`` (number, expiry, CVC, name on card) and ``totp`` (a 2FA secret:
the code is generated here, RFC 6238, when a field needs it). Logins and 2FA codes are bound to the site they
were saved for: the page's host must be that host or one of its subdomains, with the same scheme and port, so
``chase.com`` fills on ``secure.chase.com`` and never on ``chase.com.example`` or ``example.com``. Cards are
not bound to a site and fill without a question (the owner: "i dont want to be limited"); the sensitive-label
gate still stops at Pay, Place order and the like, so a purchase still needs the owner's yes.

The Keychain is driven through ``/usr/bin/security``. A secret never appears on a command line, where any
process could read it from the process table: ``security -i`` reads the add command on stdin, and the value
goes as hex so no quoting can break it. Reading uses ``find-generic-password -w``, whose output is the value.
Items ``security`` creates trust ``security`` itself, so reading them asks no question; the cost is that any
program running as the owner can read them the same way, which is the same exposure as eve's 0600 env file.
``EVE_VAULT_KEYCHAIN`` names another keychain file (the live test uses a throwaway one, never the login
keychain); ``EVE_VAULT_INDEX`` moves the index.
"""

from __future__ import annotations

import argparse
import base64
import getpass
import hashlib
import hmac
import json
import os
import re
import secrets
import struct
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional
from urllib.parse import parse_qs, unquote, urlsplit

KINDS = ("login", "card", "totp")
SERVICE = "eve-vault"
HANDLE = re.compile(r"^v_[a-z0-9]{8}$")
SECURITY = "/usr/bin/security"
MAX_LABEL_CHARS = 60
TOTP_ALGORITHMS = {"SHA1": hashlib.sha1, "SHA256": hashlib.sha256, "SHA512": hashlib.sha512}
# The parts a fill can ask for, per kind. A login's username is not secret, but it travels with the
# password so a model never has to type either.
PARTS = {"login": ("username", "password"),
         "card": ("number", "exp", "exp_month", "exp_year", "cvc", "name"),
         "totp": ("code",)}
ALL_PARTS = tuple(dict.fromkeys(p for parts in PARTS.values() for p in parts))


class VaultError(Exception):
    """One sentence for the owner or the model; never holds a value."""


class Refused(VaultError):
    """The fill was refused (another site, an unknown handle, a part the item lacks). Nothing was filled."""


def default_index() -> Path:
    configured = os.environ.get("EVE_VAULT_INDEX", "").strip()
    return Path(configured).expanduser() if configured else Path.home() / ".config" / "eve-bridge" / "vault.json"


# ---- sites -------------------------------------------------------------------------------------------------

_DEFAULT_PORTS = {"http": 80, "https": 443}


def canonical_origin(site: str) -> str:
    """``chase.com``, ``https://chase.com/login`` → ``https://chase.com``; a port only when it is not the
    scheme's own. VaultError for anything that is not a plain http(s) site."""
    raw = str(site or "").strip()
    if not raw:
        raise VaultError("a login or 2FA item needs the site it is for")
    if "://" not in raw:
        raw = "https://" + raw
    try:
        u = urlsplit(raw)
        port = u.port
    except ValueError as e:
        raise VaultError(f"not a site: {site!r}") from e
    scheme = (u.scheme or "").lower()
    host = (u.hostname or "").lower().rstrip(".")
    if scheme not in _DEFAULT_PORTS or not host or u.username or u.password or any(c.isspace() for c in raw):
        raise VaultError(f"not a site: {site!r}")
    return f"{scheme}://{host}" + (f":{port}" if port and port != _DEFAULT_PORTS[scheme] else "")


def site_allows(saved: str, page_url: str) -> bool:
    """True when ``page_url`` is on the saved site: the same scheme and port, and the saved host or a
    subdomain of it. Anything unparsable is False."""
    try:
        s, p = urlsplit(canonical_origin(saved)), urlsplit(str(page_url or ""))
        s_port, p_port = s.port, p.port
    except (VaultError, ValueError):
        return False
    p_scheme = (p.scheme or "").lower()
    if p_scheme != s.scheme or p.username or p.password:
        return False
    if (p_port or _DEFAULT_PORTS.get(p_scheme)) != (s_port or _DEFAULT_PORTS[s.scheme]):
        return False
    host, want = (p.hostname or "").lower().rstrip("."), (s.hostname or "")
    return bool(host) and (host == want or host.endswith("." + want))


def site_of(origin: str) -> str:
    """What a model is shown of a site: the host, and the port when there is one."""
    u = urlsplit(origin)
    return (u.hostname or "") + (f":{u.port}" if u.port else "")


# ---- 2FA codes (RFC 4226 / RFC 6238) ------------------------------------------------------------------------

def hotp(key: bytes, counter: int, digits: int = 6, algorithm: str = "SHA1") -> str:
    mac = hmac.new(key, struct.pack(">Q", counter), TOTP_ALGORITHMS[algorithm]).digest()
    offset = mac[-1] & 0x0F
    code = (struct.unpack(">I", mac[offset:offset + 4])[0] & 0x7FFFFFFF) % (10 ** digits)
    return str(code).zfill(digits)


def b32_key(secret: str) -> bytes:
    clean = re.sub(r"[\s-]", "", str(secret or "")).upper()
    if not clean or not re.fullmatch(r"[A-Z2-7]+=*", clean):
        raise VaultError("a 2FA secret is base32 (letters A-Z and digits 2-7)")
    return base64.b32decode(clean + "=" * (-len(clean) % 8))


def totp(secret: str, *, at: Optional[float] = None, digits: int = 6, period: int = 30,
         algorithm: str = "SHA1") -> str:
    """The code for ``secret`` (base32) at ``at`` (now by default)."""
    now = time.time() if at is None else at
    return hotp(b32_key(secret), int(now // period), digits, algorithm)


def parse_totp(text: str) -> dict[str, Any]:
    """A 2FA secret as the owner pastes it: an ``otpauth://totp/...`` URI (from a QR code) or the bare base32
    secret. The stored form: {secret, digits, period, algorithm}."""
    raw = str(text or "").strip()
    out: dict[str, Any] = {"digits": 6, "period": 30, "algorithm": "SHA1"}
    if raw.lower().startswith("otpauth://"):
        u = urlsplit(raw)
        if u.netloc.lower() != "totp":
            raise VaultError("only time-based (totp) codes are supported")
        q = {k.lower(): v[0] for k, v in parse_qs(u.query).items()}
        raw = unquote(q.get("secret", ""))
        try:
            out["digits"] = int(q.get("digits", 6))
            out["period"] = int(q.get("period", 30))
        except ValueError as e:
            raise VaultError("the otpauth URI's digits or period is not a number") from e
        out["algorithm"] = q.get("algorithm", "SHA1").upper()
    if out["algorithm"] not in TOTP_ALGORITHMS or out["digits"] not in (6, 7, 8) or not 10 <= out["period"] <= 120:
        raise VaultError("unsupported 2FA settings (digits 6-8, period 10-120 s, SHA1/SHA256/SHA512)")
    b32_key(raw)                                                    # raises when it is not base32
    out["secret"] = re.sub(r"[\s-]", "", raw).upper()
    return out


# ---- the Keychain ---------------------------------------------------------------------------------------------

class Keychain:
    """Generic-password items under SERVICE, one per handle; the value is hex of a JSON object."""

    def __init__(self, keychain: str = "", run: Callable[..., Any] = subprocess.run) -> None:
        self.keychain = keychain if keychain else os.environ.get("EVE_VAULT_KEYCHAIN", "").strip()
        if self.keychain and any(c.isspace() or c in "\"'\\" for c in self.keychain):
            raise VaultError("EVE_VAULT_KEYCHAIN must be a path without spaces or quotes")
        self.run = run

    def _tail(self) -> list[str]:
        return [self.keychain] if self.keychain else []

    def put(self, handle: str, value: dict[str, Any]) -> None:
        if not HANDLE.match(handle):
            raise VaultError("bad handle")
        blob = json.dumps(value, separators=(",", ":")).encode().hex()
        # `security -i` reads its command from stdin: the value is never on a command line.
        line = " ".join(["add-generic-password", "-U", "-s", SERVICE, "-a", handle, "-l", SERVICE, "-w", blob,
                         *self._tail()]) + "\n"
        r = self.run([SECURITY, "-i"], input=line, capture_output=True, text=True, timeout=20)
        if r.returncode != 0 or "error" in (r.stderr or "").lower():
            raise VaultError("the Keychain refused the item")
        if self.get(handle) != value:
            raise VaultError("the Keychain did not keep the item")

    def get(self, handle: str) -> Optional[dict[str, Any]]:
        r = self.run([SECURITY, "find-generic-password", "-s", SERVICE, "-a", handle, "-w", *self._tail()],
                     capture_output=True, text=True, timeout=20)
        if r.returncode != 0:
            return None
        try:
            value = json.loads(bytes.fromhex((r.stdout or "").strip()).decode())
        except ValueError:
            return None
        return value if isinstance(value, dict) else None

    def delete(self, handle: str) -> bool:
        r = self.run([SECURITY, "delete-generic-password", "-s", SERVICE, "-a", handle, *self._tail()],
                     capture_output=True, text=True, timeout=20)
        return r.returncode == 0


# ---- the vault ------------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Item:
    handle: str
    kind: str
    label: str
    origin: str = ""                  # login / totp: the site it fills on; card: ""

    def line(self) -> str:
        """All a model is ever told of an item."""
        where = f" for {site_of(self.origin)}" if self.origin else ""
        return f'{self.handle} {self.kind} "{self.label}"{where}'

    def to_dict(self) -> dict[str, str]:
        return {"handle": self.handle, "kind": self.kind, "label": self.label, "origin": self.origin}


def _clean_label(label: str) -> str:
    text = " ".join(str(label or "").replace('"', "'").split())[:MAX_LABEL_CHARS]
    if not text:
        raise VaultError("an item needs a label")
    return text


def _card(value: dict[str, Any]) -> dict[str, str]:
    number = re.sub(r"[\s-]", "", str(value.get("number") or ""))
    if not re.fullmatch(r"\d{12,19}", number):
        raise VaultError("a card number is 12 to 19 digits")
    month, year = str(value.get("exp_month") or "").strip(), str(value.get("exp_year") or "").strip()
    if not (month.isdigit() and 1 <= int(month) <= 12 and year.isdigit() and len(year) in (2, 4)):
        raise VaultError("a card needs its expiry month (1-12) and year")
    cvc = str(value.get("cvc") or "").strip()
    if not re.fullmatch(r"\d{3,4}", cvc):
        raise VaultError("a CVC is 3 or 4 digits")
    return {"number": number, "exp_month": month.zfill(2), "exp_year": year if len(year) == 4 else "20" + year,
            "cvc": cvc, "name": " ".join(str(value.get("name") or "").split())}


class Vault:
    def __init__(self, index: Optional[Path] = None, keychain: Optional[Keychain] = None,
                 clock: Callable[[], float] = time.time) -> None:
        self.index = Path(index) if index is not None else default_index()
        self.keychain = keychain if keychain is not None else Keychain()
        self.clock = clock

    def items(self) -> list[Item]:
        try:
            raw = json.loads(self.index.read_text())
        except (OSError, ValueError):
            return []
        out = []
        for d in raw.get("items", []) if isinstance(raw, dict) else []:
            try:
                if HANDLE.match(d["handle"]) and d["kind"] in KINDS:
                    out.append(Item(d["handle"], d["kind"], str(d["label"]), str(d.get("origin") or "")))
            except (KeyError, TypeError):
                continue
        return out

    def get(self, handle: str) -> Optional[Item]:
        return next((i for i in self.items() if i.handle == str(handle or "").strip()), None)

    def lines(self, limit: int = 40) -> list[str]:
        return [i.line() for i in self.items()[:limit]]

    def _write(self, items: list[Item]) -> None:
        self.index.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.index.with_name(self.index.name + ".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump({"items": [i.to_dict() for i in items]}, f, indent=1)
        os.chmod(tmp, 0o600)
        os.replace(tmp, self.index)

    def add(self, kind: str, label: str, value: dict[str, Any], site: str = "") -> Item:
        """Store an item; returns it (no value). The value is checked for its kind first."""
        if kind not in KINDS:
            raise VaultError(f"the kinds are {', '.join(KINDS)}")
        label = _clean_label(label)      # before the Keychain: a bad label must not leave an unindexed secret
        if kind == "login":
            if not str(value.get("password") or ""):
                raise VaultError("a login needs a password")
            value = {"username": str(value.get("username") or ""), "password": str(value["password"])}
        elif kind == "card":
            value = _card(value)
        else:
            value = parse_totp(str(value.get("secret") or ""))
        origin = canonical_origin(site) if kind in ("login", "totp") else ""
        taken = {i.handle for i in self.items()}
        while True:
            handle = "v_" + "".join(secrets.choice("abcdefghijklmnopqrstuvwxyz0123456789") for _ in range(8))
            if handle not in taken:
                break
        self.keychain.put(handle, value)
        item = Item(handle, kind, label, origin)
        self._write(self.items() + [item])
        return item

    def remove(self, handle: str) -> bool:
        items = self.items()
        keep = [i for i in items if i.handle != handle]
        if len(keep) == len(items):
            return False
        self.keychain.delete(handle)
        self._write(keep)
        return True

    def fill_values(self, handle: str, page_url: str, part: str = "") -> tuple[Item, dict[str, str]]:
        """The values a fill may type on ``page_url``: every part of the item, or just ``part``. Refused when
        the handle is unknown, the item is bound to another site, or it has no such part. The only reader of
        values in eve; its result goes to an effector, never into a prompt, a log or a reply."""
        item = self.get(handle)
        if item is None:
            raise Refused(f"no vault item {str(handle)[:20]!r}")
        if item.origin and not site_allows(item.origin, page_url):
            raise Refused(f'"{item.label}" is saved for {site_of(item.origin)}, not this page')
        if part and part not in PARTS[item.kind]:
            raise Refused(f"a {item.kind} has no {part!r}; its parts are {', '.join(PARTS[item.kind])}")
        value = self.keychain.get(handle)
        if value is None:
            raise Refused(f'"{item.label}" is missing from the Keychain')
        if item.kind == "login":
            out = {"username": str(value.get("username") or ""), "password": str(value.get("password") or "")}
        elif item.kind == "card":
            out = {"number": value["number"], "exp_month": value["exp_month"], "exp_year": value["exp_year"],
                   "exp": f"{value['exp_month']}/{value['exp_year'][-2:]}", "cvc": value["cvc"],
                   "name": value.get("name") or ""}
        else:
            out = {"code": totp(value["secret"], at=self.clock(), digits=int(value.get("digits", 6)),
                                period=int(value.get("period", 30)), algorithm=str(value.get("algorithm", "SHA1")))}
        out = {k: v for k, v in out.items() if v}
        if part:
            if part not in out:
                raise Refused(f'"{item.label}" has no {part} saved')
            out = {part: out[part]}
        return item, out


# ---- eve-bridge vault ---------------------------------------------------------------------------------------

def cli(argv: list[str], *, vault: Optional[Vault] = None, ask: Callable[[str], str] = input,
        ask_secret: Callable[[str], str] = getpass.getpass, out: Any = None) -> int:
    """``eve-bridge vault add login|card|totp``, ``list``, ``remove HANDLE``. Secrets are read with getpass at
    the terminal: never from an argument, never from chat."""
    out = out or sys.stdout
    p = argparse.ArgumentParser(prog="eve-bridge vault", description="Sign-ins, cards and 2FA secrets eve "
                                "fills for you without a model ever seeing them.")
    sub = p.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("add", help="Save an item (values are asked for at the terminal)")
    a.add_argument("kind", choices=KINDS)
    a.add_argument("--label", required=True, help='What eve calls it, e.g. "Chase" or "Visa ending 4242"')
    a.add_argument("--site", default="", help="login / totp: the site it fills on, e.g. chase.com")
    sub.add_parser("list", help="Show the items (never their values)")
    r = sub.add_parser("remove", help="Delete an item from the vault and the Keychain")
    r.add_argument("handle")
    args = p.parse_args(argv)
    v = vault or Vault()
    try:
        if args.cmd == "list":
            items = v.items()
            print("\n".join(i.line() for i in items) if items else "The vault is empty.", file=out)
            return 0
        if args.cmd == "remove":
            ok = v.remove(args.handle)
            print("Removed." if ok else f"No item {args.handle!r}.", file=out)
            return 0 if ok else 1
        if args.kind in ("login", "totp") and not args.site:
            p.error(f"a {args.kind} needs --site")
        if args.kind == "login":
            value: dict[str, Any] = {"username": ask("Username: ").strip(), "password": ask_secret("Password: ")}
        elif args.kind == "card":
            exp = ask("Expiry (MM/YY): ").strip()
            month, _, year = exp.partition("/")
            value = {"number": ask_secret("Card number: "), "exp_month": month.strip(), "exp_year": year.strip(),
                     "cvc": ask_secret("CVC: "), "name": ask("Name on card: ").strip()}
        else:
            value = {"secret": ask_secret("2FA secret or otpauth:// URI: ")}
        item = v.add(args.kind, args.label, value, site=args.site)
    except VaultError as e:
        print(f"vault: {e}", file=sys.stderr)
        return 1
    print(f"Saved {item.line()}", file=out)
    return 0
