"""lite_web.LanServer: the door phones and laptops on the same Wi-Fi use (docs/plans/eve-lite.md, M4 and M5).

Real TLS with the rig's own local CA (lite_tls.LocalTls) on loopback ports, and clients that trust that CA, as a phone
that installed it does. The rig is test_lite_web_local.Rig: the Mac's page adds the phone, the phone pairs here."""
from __future__ import annotations

import asyncio
import contextlib
import json
import plistlib
import ssl
from pathlib import Path
from typing import Any

import pytest
from test_lite_web_local import (
    HOSTNAME,
    IPHONE,
    LAN_IP,
    Rig,
    cookie_of,
    http,
    run,
    send_raw,
    until,
)

from eve_bridge import lan_net, lite_web, telegram, web_auth

WINDOW_GONE = lite_web.NO_WINDOW


@pytest.fixture(autouse=True)
def _plain_logging():
    """The sign-ins hand their key or token to telegram.hide_token, which redacts it in every log record for the
    rest of the process: undone after each test, as test_onboarding does."""
    yield
    telegram.unhide_tokens()


def client_hello() -> bytes:
    """The first bytes a TLS client sends: a real ClientHello, from Python's own TLS."""
    ctx = ssl.create_default_context()
    incoming, outgoing = ssl.MemoryBIO(), ssl.MemoryBIO()
    tls = ctx.wrap_bio(incoming, outgoing, server_hostname="localhost")
    with contextlib.suppress(ssl.SSLWantReadError):
        tls.do_handshake()
    hello = outgoing.read()
    assert hello[:1] == b"\x16"                                          # a handshake record
    return hello


async def answer_to(port: int, data: bytes) -> bytes:
    """What the door sends back for ``data``: b"" when it closes without a byte, a reset included."""
    try:
        return await send_raw(port, data, secs=5.0)
    except (ConnectionResetError, BrokenPipeError):
        return b""


# ---- pairing ----------------------------------------------------------------------------------------------------

def test_a_phone_pairs_with_the_qr_codes_secret_and_gets_a_secure_cookie(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            opened = await r.open_pairing(await r.login())
            secret = opened["links"][0]["url"].split("#p=", 1)[1]
            paired = await r.lan("POST", lite_web.PAIR_PATH, {"secret": secret}, headers={"User-Agent": IPHONE})
            assert paired.status == 200 and paired.json() == {"ok": True}
            token = cookie_of(paired, web_auth.COOKIE_DEV)
            assert paired.cookies == [f"eve_dev={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=34560000"]
            assert not r.ctx.pairing.is_open                                  # one device per window
            status = await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={token}")
            assert status.status == 200 and status.json()["state"] == "READY"
            assert cookie_of(status, web_auth.COOKIE_DEV) == token            # set again: 400 days from this visit
            phone = next(d for d in r.ctx.devices.list() if d["label"] == "iPhone · Safari")
            assert phone["host"] == "localhost"
            await until(lambda: r.ctx.shared.local_network == "ok")
            assert r.probes and r.probes[0][0] == "127.0.0.1"                 # the Local Network probe, at pairing
            assert status.json()["local_network"] in ("unknown", "ok")
            assert (await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={token}")).json()["local_network"] == "ok"
            again = await r.lan("POST", lite_web.PAIR_PATH, {"secret": secret})
            assert again.status == 403 and again.json()["error"] == WINDOW_GONE and again.cookies == []

    run(go())


def test_a_laptop_pairs_with_the_six_digit_code_and_five_wrong_tries_close_the_window(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = await r.login()
            code = (await r.open_pairing(mac))["code"]
            wrong = f"{(int(code) + 1) % 1_000_000:06d}"
            for _ in range(lite_web.web_auth.PAIR_TRIES - 1):
                reply = await r.lan("POST", lite_web.PAIR_PATH, {"code": wrong})
                assert reply.status == 403 and reply.json()["error"] == lite_web.WRONG_CODE and reply.cookies == []
            last = await r.lan("POST", lite_web.PAIR_PATH, {"code": wrong})
            assert last.status == 429 and last.json()["error"] == lite_web.TOO_MANY
            closed = await r.lan("POST", lite_web.PAIR_PATH, {"code": code})  # the right code, too late
            assert closed.status == 403 and closed.json()["error"] == WINDOW_GONE and closed.cookies == []
            code = (await r.open_pairing(mac))["code"]
            both = await r.lan("POST", lite_web.PAIR_PATH, {"code": code, "secret": "x" * 22})
            assert both.status == 403                                         # both at once is a wrong try
            typed = await r.lan("POST", lite_web.PAIR_PATH, {"code": f"{code[:3]} {code[3:]}"})
            assert typed.status == 200 and cookie_of(typed, web_auth.COOKIE_DEV)

    run(go())


def test_a_new_window_replaces_the_old_one(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = await r.login()
            first = (await r.open_pairing(mac))["links"][0]["url"].split("#p=", 1)[1]
            second = (await r.open_pairing(mac))["links"][0]["url"].split("#p=", 1)[1]
            assert first != second
            assert (await r.lan("POST", lite_web.PAIR_PATH, {"secret": first})).status == 403
            assert (await r.lan("POST", lite_web.PAIR_PATH, {"secret": second})).status == 200

    run(go())


def test_no_window_means_no_pairing(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            await r.open_pairing(await r.login())
            r.ctx.pairing.close()
            reply = await r.lan("POST", lite_web.PAIR_PATH, {"code": "123456"})
            assert reply.status == 403 and reply.json()["error"] == WINDOW_GONE and reply.cookies == []
            assert [d for d in r.ctx.devices.list() if not d["label"].startswith("This Mac")] == []

    run(go())


# ---- what the LAN door has, and what it does not --------------------------------------------------------------

MAC_ONLY = [("GET", lite_web.ONBOARD_STATE_PATH), ("GET", lite_web.OPENROUTER_START_PATH),
            ("GET", lite_web.OPENROUTER_CALLBACK_PATH + "?code=c&state=s"), ("GET", lite_web.COMPOSIO_START_PATH),
            ("GET", lite_web.COMPOSIO_CALLBACK_PATH + "?code=c&state=s"), ("POST", lite_web.COMPOSIO_SKIP_PATH),
            ("POST", lite_web.MAC_LOGIN_PATH), ("POST", lite_web.PAIR_OPEN_PATH), ("GET", lite_web.DEVICES_PATH),
            ("POST", lite_web.REVOKE_PATH), ("POST", lite_web.LAN_PATH), ("GET", "/onboard/anything"),
            ("GET", "/api/mac/anything")]


def test_the_lan_door_has_no_setup_and_none_of_the_macs_routes(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            phone = await r.pair()
            for method, path in MAC_ONLY:
                body = {"code": r.ctx.mac_login.issue(), "id": "all", "on": False} if method == "POST" else None
                reply = await r.lan(method, path, body, cookie=f"eve_dev={phone}")
                assert reply.status == 404, (method, path, reply.status)
                assert reply.json() == {"ok": False, "error": lite_web.NOT_FOUND} and reply.cookies == []
            assert r.provider_calls == [] and r.restarts == 0                 # no sign-in started or finished
            assert r.local.lan_on() and len(r.ctx.devices.list()) == 2        # nothing turned off or revoked
            assert (await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={phone}")).status == 200   # control

    run(go())


def test_the_ca_downloads_carry_no_key_and_are_the_lan_doors_only(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            await r.open_pairing(await r.login())
            pem = await r.lan("GET", lite_web.CA_PATH)                        # before pairing: no cookie
            assert pem.status == 200 and pem.headers["content-type"] == "application/x-x509-ca-cert"
            assert pem.headers["content-disposition"] == 'attachment; filename="eve-lite-ca.crt"'
            assert pem.body == r.ctx.tls.ca_pem() and pem.body.startswith(b"-----BEGIN CERTIFICATE-----")
            profile = await r.lan("GET", lite_web.MOBILECONFIG_PATH)
            assert profile.status == 200 and profile.headers["content-type"] == "application/x-apple-aspen-config"
            parsed = plistlib.loads(profile.body)
            assert parsed["PayloadType"] == "Configuration"
            assert [p["PayloadType"] for p in parsed["PayloadContent"]] == ["com.apple.security.root"]
            for body in (pem.body, profile.body):
                assert b"PRIVATE KEY" not in body
            assert b"PRIVATE KEY" in (tmp_path / "web-tls" / "ca.key").read_bytes()   # positive control: it exists
            for path in (lite_web.CA_PATH, lite_web.MOBILECONFIG_PATH):
                assert (await r.mac("GET", path)).status == 404               # the Mac's page has no use for them

    run(go())


@pytest.mark.parametrize("host, ok", [
    ("test-mac.local", True), ("TEST-MAC.LOCAL", True), (LAN_IP, True), ("localhost", True), ("127.0.0.1", True),
    ("evil.example", False), ("test-mac.local.evil.example", False), ("192.168.50.8", False), ("[::1]", False),
])
def test_the_lan_hosts_are_the_certificates_names_and_the_macs_addresses(tmp_path: Path, host: str, ok: bool) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            await r.open_pairing(await r.login())
            port = r.ctx.shared.lan.port
            reply = await r.lan("GET", lite_web.HEALTH_PATH, host=f"{host}:{port}")
            assert reply.status == (200 if ok else 421), (host, reply.status)
            if ok:
                for wrong in (host, f"{host}:{port + 1}"):                     # no port, or another
                    assert (await r.lan("GET", lite_web.HEALTH_PATH, host=wrong)).status == 421

    run(go())


def test_a_new_address_reissues_the_certificate_at_add_a_phone(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = await r.login()
            await r.open_pairing(mac)
            port = r.ctx.shared.lan.port
            first = r.ctx.tls.leaf_cert_path.read_bytes()
            assert (await r.lan("GET", lite_web.HEALTH_PATH, host=f"192.168.50.8:{port}")).status == 421
            r.ctx.addresses = lambda: ["192.168.50.8"]                        # DHCP gave the Mac a new address
            opened = await r.open_pairing(mac)
            assert opened["links"][1]["url"].startswith(f"https://192.168.50.8:{port}/#p=")
            assert r.ctx.tls.leaf_cert_path.read_bytes() != first             # reissued with it
            assert {"192.168.50.8", LAN_IP, "test-mac.local"} <= r.ctx.tls.names()   # the old one kept too
            r.ctx.shared.lan._allowed_at = 0.0                                 # past NAMES_TTL_SECS
            assert (await r.lan("GET", lite_web.HEALTH_PATH, host=f"192.168.50.8:{port}")).status == 200
            unchanged = r.ctx.tls.leaf_cert_path.read_bytes()
            await r.open_pairing(mac)                                         # nothing new: no reissue
            assert r.ctx.tls.leaf_cert_path.read_bytes() == unchanged

    run(go())


# ---- TLS, and the peer filter before it ------------------------------------------------------------------------

def test_the_lan_door_speaks_tls_1_2_or_later_with_the_local_cas_certificate(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            await r.open_pairing(await r.login())
            port = r.ctx.shared.lan.port
            reader, writer = await asyncio.open_connection("127.0.0.1", port, ssl=r.trust(),
                                                           server_hostname=f"{HOSTNAME.lower()}.local")
            version = writer.get_extra_info("ssl_object").version()
            assert version in ("TLSv1.2", "TLSv1.3")
            writer.write(f"GET /healthz HTTP/1.1\r\nHost: test-mac.local:{port}\r\n\r\n".encode())
            assert (await asyncio.wait_for(reader.read(), 10)).startswith(b"HTTP/1.1 200")
            writer.close()
            # a browser that has not installed the CA sees an unknown certificate (the click-through warning)
            with pytest.raises(ssl.SSLCertVerificationError):
                await asyncio.open_connection("127.0.0.1", port, ssl=ssl.create_default_context(),
                                              server_hostname="localhost")
            # plain HTTP to the LAN door gets no HTTP back
            plain = await answer_to(port, f"GET /healthz HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n".encode())
            assert b"HTTP/1.1" not in plain

    run(go())


def test_the_peer_filter_admits_home_networks_and_refuses_the_rest() -> None:
    for peer in ("127.0.0.1", ("192.168.1.20", 50000), "10.1.2.3", "172.16.5.4", "169.254.3.3", "fe80::1%en0",
                 ("fd12:3456::1", 50000, 0, 0), "::ffff:192.168.1.9"):
        assert lan_net.peer_allowed(peer), peer
    for peer in ("8.8.8.8", ("1.1.1.1", 443), "100.64.0.1", "2001:4860:4860::8888", "198.51.100.7", "", None,
                 "not an address"):
        assert not lan_net.peer_allowed(peer), peer


def test_a_peer_from_outside_is_dropped_before_tls(tmp_path: Path) -> None:
    async def go() -> None:
        allow = [True]
        async with Rig(tmp_path, peer_allowed=lambda peer: allow[0]) as r:
            await r.open_pairing(await r.login())
            lan = r.ctx.shared.lan
            port = lan.port
            real_context = r.ctx.tls.context
            contexts: list[int] = []

            def counted() -> ssl.SSLContext:
                contexts.append(1)
                return real_context()

            r.ctx.tls.context = counted
            allow[0] = False
            hello = client_hello()
            assert await answer_to(port, hello) == b""                        # closed, not a byte back
            assert await answer_to(port, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n") == b""
            assert lan.dropped == 2 and contexts == []                        # no TLS work at all for them
            with pytest.raises((ssl.SSLError, ConnectionError, OSError)):
                await http(port, "GET", lite_web.HEALTH_PATH, tls=r.trust())
            allow[0] = True                                                   # positive control: a home peer
            assert (await http(port, "GET", lite_web.HEALTH_PATH, tls=r.trust())).status == 200
            assert contexts == [1] and lan.dropped == 3

    run(go())


def test_rtc_through_the_lan_door_passes_the_phones_address_and_probes_it(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            phone = await r.pair()
            r.probes.clear()
            answer = await r.lan("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\noffer"}, cookie=f"eve_dev={phone}")
            assert answer.status == 200 and answer.json()["voice"] is True
            device_id, sdp, peer = r.rtc.offers[-1]
            assert sdp == "v=0\r\noffer" and peer == "127.0.0.1"
            assert r.ctx.shared.rtc_doors[device_id] == "lan"
            await until(lambda: bool(r.probes))
            assert r.probes[0][0] == "127.0.0.1"

    run(go())


def test_the_chat_over_https_reaches_the_same_linkchat(tmp_path: Path) -> None:
    async def go() -> None:
        from test_link_chat import FakeInlet, events, store_at, two_lines
        from test_lite_web_local import link_door

        inlet = FakeInlet(store_at(tmp_path), two_lines)
        async with Rig(tmp_path / "rig", link=link_door(tmp_path, inlet)) as r:
            phone = await r.pair()
            turn = await r.lan("POST", "/api/web/link/turn", {"text": "hi from the phone", "client_id": "lan-turn-1"},
                               cookie=f"eve_dev={phone}")
            assert turn.status == 200
            contents, done, _ = events(turn.body)
            assert contents == ["First line.", "\n\nSecond line."] and done
            assert inlet.heard == [("hi from the phone", False, "lan-turn-1")]

    run(go())


def test_a_lan_door_started_on_its_own_still_checks_everything(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            lan = lite_web.LanServer(r.ctx)
            port = await lan.start()
            try:
                assert r.ctx.shared.lan is lan and r.local.lan_on()
                assert (await http(port, "GET", lite_web.HEALTH_PATH, tls=r.trust())).status == 200
                assert (await http(port, "GET", lite_web.STATUS_PATH, tls=r.trust())).status == 401
                assert (await http(port, "GET", lite_web.HEALTH_PATH, tls=r.trust(), host="evil.example:1")).status == 421
                assert json.loads((tmp_path / "web.json").read_text())["lan_port"] == port
            finally:
                await lan.stop()
            assert not r.local.lan_on()

    run(go())


def test_an_unusable_certificate_keeps_phones_out_and_says_so(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            await r.open_pairing(await r.login())
            port = r.ctx.shared.lan.port

            def broken() -> Any:
                raise OSError("the certificate's folder is gone")

            r.ctx.tls.context = broken
            assert await answer_to(port, client_hello()) == b""
            assert "the LAN certificate could not be loaded (OSError)" in caplog.text

    run(go())
