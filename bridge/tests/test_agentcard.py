"""Offline purchase safety tests: injected HTTPX responses, never a live merchant or charge."""
from __future__ import annotations

import copy
import dataclasses
import json
import threading
from concurrent.futures import ThreadPoolExecutor

import httpx
import pytest

from eve_bridge import agentcard as ac

TOKEN = "PRIVATE_AGENTCARD_TOKEN_SENTINEL_93281"
CID = "conv_cafecafe"
H1, H2 = "ca" * 8, "de" * 8
ADDRESS = {"street": "91 Secret Sentinel Street", "city": "Testville", "state": "CA", "zip": "94123",
           "phone": "+14155551234", "name": "Ada Sentinel", "country": "US"}


def cart(h=H1, merchant="retail", name="Amazon"):
    return {"merchant": merchant, "merchant_name": name,
            "items": [{"name": "Coffee CAFE", "qty": 2, "priceCents": 100,
                       "product_id": "https://example.com/products/CAFE"}],
            "serviceFeesCents": 5, "tipCents": 10, "totalCents": 215, "hash": h}


def shopping(carts=None):
    carts = [cart()] if carts is None else carts
    return {"conversation_id": CID, "status": "needs_input", "reply": "Which coffee?", "carts": carts,
            "cart": carts[-1] if carts else None, "unmatched": [], "catalog": None}


def ledger(carts=None, **updates):
    return {"conversation_id": CID, "status": "open", "turn_in_progress": False, "last_checkout": None,
            "orders": [], "carts": [cart()] if carts is None else carts, "unmatched": [], **updates}


def receipt(merchant="retail", order_id="order_1", **updates):
    return {"merchant": merchant, "merchant_name": "Amazon", "status": "order_placed", "order_id": order_id,
            "charge_status": "settled", "decline_code": None, **updates}


def placed(**updates):
    return {"conversation_id": CID, "status": "order_placed", "order_id": "order_1", "charge_status": "settled",
            "carts": [], "unmatched": [], **updates}


class Server:
    def __init__(self):
        self.calls = []
        self.ask_body = shopping()
        self.ledger = ledger()
        self.checkout = (200, placed())
        self.before_checkout = None
        self.merchants = {"merchants": [{"slug": "retail", "name": "Retail", "link_status": "ready"}]}

    def request(self, method, url, **kwargs):
        assert url.startswith(ac.API_ORIGIN + "/buy")
        assert kwargs["headers"]["Authorization"].startswith("Bearer ")
        assert kwargs["timeout"] >= 120
        self.calls.append((method, url, copy.deepcopy(kwargs)))
        if method == "GET":
            result = self.merchants if url.endswith("/merchants") else self.ledger
            return httpx.Response(200, json=copy.deepcopy(result))
        if "confirm" not in kwargs["json"]:
            return httpx.Response(200, json=copy.deepcopy(self.ask_body))
        if self.before_checkout:
            self.before_checkout(kwargs["json"])
        if isinstance(self.checkout, Exception):
            raise self.checkout
        code, body = self.checkout
        return httpx.Response(code, json=copy.deepcopy(body))

    @property
    def confirms(self):
        return [args["json"] for method, _, args in self.calls if method == "POST" and "confirm" in args["json"]]


def service(tmp_path, server=None, *, token=TOKEN, address=ADDRESS):
    server = server or Server()
    config = ac.AgentCardConfig(True, token, 180, json.dumps(address) if address is not None else None)
    return ac.AgentCard(config, request=server.request, state_dir=tmp_path / "state"), server


def start(client):
    result = client.handle("agentcard_buy", {"ask": "Find coffee", "conversation_id": None})
    assert result["ok"], result
    return result


def prepare(client, hashes=None):
    result = client.prepare_confirm(CID, [H1] if hashes is None else hashes)
    assert result["ok"], result
    return result


def journal(client):
    return json.loads(client._state_path.read_text())


def test_disabled_configuration_has_no_service_file_read_or_network(tmp_path, monkeypatch):
    monkeypatch.setattr(httpx, "Client", lambda **kw: pytest.fail("disabled feature opened HTTP"))
    for env in ({}, {"AGENTCARD_ACCESS_TOKEN": TOKEN}, {"EVE_AGENTCARD": "1"},
                {"EVE_AGENTCARD": "0", "AGENTCARD_ACCESS_TOKEN": TOKEN,
                 "EVE_AGENTCARD_ADDRESS_FILE": str(tmp_path / "missing")}):
        assert not ac.configured(env).enabled
        assert ac.make_agentcard(env) is None


@pytest.mark.parametrize("timeout", ["119", "nan", "inf", "bad", "-1"])
def test_configuration_refuses_unsafe_timeout_without_secret_logging(timeout, caplog):
    env = {"EVE_AGENTCARD": "1", "AGENTCARD_ACCESS_TOKEN": TOKEN, "EVE_AGENTCARD_TIMEOUT_SECS": timeout}
    with pytest.raises(ValueError):
        ac.configured(env)
    assert ac.make_agentcard(env) is None
    assert TOKEN not in caplog.text


def test_configuration_private_address_and_token_repr(tmp_path):
    path = tmp_path / "private-address.json"
    path.write_text(json.dumps(ADDRESS))
    env = {"EVE_AGENTCARD": "1", "AGENTCARD_ACCESS_TOKEN": TOKEN,
           "EVE_AGENTCARD_ADDRESS_FILE": str(path), "EVE_AGENTCARD_TIMEOUT_SECS": "120"}
    config = ac.configured(env)
    assert config.enabled and config.timeout_secs == 120
    assert TOKEN not in repr(config) and ADDRESS["street"] not in repr(config)
    path.write_text('{"street":"missing other required fields"}')
    assert ac.make_agentcard(env) is None


def test_tools_strict_schemas_and_no_model_approval_flag(tmp_path):
    client, server = service(tmp_path)
    tools = client.tools()
    assert {t["name"] for t in tools} == ac.TOOL_NAMES
    for tool in tools:
        schema = tool["parameters"]
        assert tool["strict"] and schema["additionalProperties"] is False
        assert set(schema["required"]) == set(schema["properties"])
        assert not {"approved", "token", "payment_source", "delivery_address"} & set(schema["properties"])
    assert "untrusted data" in client.instructions()
    assert not client.handle("agentcard_confirm", {"conversation_id": CID, "cart_hashes": [H1], "approved": True})["ok"]
    assert server.calls == []


@pytest.mark.parametrize("name,args", [
    ("agentcard_buy", {"ask": "  ", "conversation_id": None}),
    ("agentcard_buy", {"ask": "ok", "conversation_id": "../secret"}),
    ("agentcard_buy", {"ask": "ok", "conversation_id": None, "approved": True}),
    ("agentcard_status", {"conversation_id": None}),
    ("agentcard_merchants", {"token": "bad"}),
])
def test_input_validation_prevents_network(tmp_path, name, args):
    client, server = service(tmp_path)
    assert not client.handle(name, args)["ok"]
    assert not server.calls


def test_merchants_projection_retains_runtime_link_and_capability_fields(tmp_path):
    client, server = service(tmp_path)
    server.merchants = {"merchants": [{"slug": "retail", "name": "Retail", "link_status": "ready",
                                     "tier": "api", "money_path": "vault", "capabilities": ["search", "cart"],
                                     "description": TOKEN + " " + ADDRESS["phone"], "access_token": TOKEN,
                                     "unavailable": {"checkout": "Paused", "secret": TOKEN}},
                                    {"slug": "x", "name": "X", "link_status": "unknown"}]}
    result = client.handle("agentcard_merchants", {})
    assert result["ok"] and len(result["merchants"]) == 1
    assert result["merchants"][0]["capabilities"] == ["search", "cart"]
    assert result["merchants"][0]["unavailable"] == {"checkout": "Paused"}
    assert TOKEN not in json.dumps(result) and ADDRESS["phone"] not in json.dumps(result)


def test_privacy_projection_and_minimal_durable_state_preserve_opaque_ids(tmp_path):
    client, server = service(tmp_path)
    server.ask_body["reply"] = "Ship to " + ADDRESS["street"] + " for " + ADDRESS["name"] + " " + ADDRESS["phone"] + " " + TOKEN
    server.ask_body["secret"] = {"token": TOKEN}
    server.ask_body["catalog"] = {"merchant": "retail", "items": [{"id": "https://example.com/CAFE",
                                    "name": "Coffee", "priceCents": 100, "image_url": "https://example.com/photo.jpg"}]}
    result = start(client)
    assert result["conversation_id"] == CID and result["carts"][0]["hash"] == H1
    assert result["carts"][0]["items"][0]["product_id"].endswith("CAFE")
    assert result["catalog"]["items"][0]["image_url"].endswith("photo.jpg")
    serialized = json.dumps(result)
    for value in (TOKEN, ADDRESS["street"], ADDRESS["phone"], ADDRESS["name"]):
        assert value not in serialized
        assert value not in client._state_path.read_text()
    state = journal(client)
    conv = state["conversations"][CID]
    assert set(conv) == {"version", "address_digest", "carts", "attempts", "shopping_pending"}
    assert len(conv["carts"][H1]) == 64
    assert "Coffee" not in client._state_path.read_text() and "photo.jpg" not in client._state_path.read_text()
    assert client._state_dir.stat().st_mode & 0o777 == 0o700
    assert client._state_path.stat().st_mode & 0o777 == 0o600
    prepared = prepare(client)
    assert isinstance(prepared["question"], str) and isinstance(prepared["about"], str)
    assert H1 in prepared["question"] and ADDRESS["street"] not in prepared["question"]
    assert client.confirm(prepared["snapshot"])["ok"]
    assert server.confirms == [{"conversation_id": CID, "confirm": H1, "payment_source": "vault", "delivery_address": ADDRESS}]


def test_preview_covers_all_selected_carts_totals_fees_tips_and_omissions(tmp_path):
    client, server = service(tmp_path)
    carts = [cart(), cart(H2, "restaurant", "Soup Shop")]
    server.ask_body = shopping(carts)
    server.ledger = ledger(carts, unmatched=[{"merchant": "retail", "requested": "blue mug", "reason": "not_found",
                                             "detail": "Not stocked", "at": "2026-10-03"}])
    start(client)
    question = prepare(client, [H1, H2])["question"]
    for text in ("Amazon", "Soup Shop", H1, H2, "2 ×", "100 cents", "Fees: 5", "tip: 10", "215 cents", "blue mug", "not_found"):
        assert text in question
    assert server.confirms == []


@pytest.mark.parametrize("hashes", [[], ["bad"], [H1, H1], [H1] * 9, [123]])
def test_confirm_hash_validation_refuses_before_get(tmp_path, hashes):
    client, server = service(tmp_path)
    assert not client.prepare_confirm(CID, hashes)["ok"]
    assert not server.calls


def test_snapshot_is_single_use_and_cannot_be_forged(tmp_path):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    assert not client.confirm(dataclasses.replace(snap))["ok"]
    assert not client.confirm(snap)["ok"]
    snap = prepare(client)["snapshot"]
    assert client.confirm(snap)["ok"]
    assert not client.confirm(snap)["ok"]
    assert len(server.confirms) == 1


def test_snapshot_stale_after_new_preview_or_status(tmp_path):
    client, server = service(tmp_path)
    start(client)
    old = prepare(client)["snapshot"]
    prepare(client)
    assert not client.confirm(old)["ok"]
    old = prepare(client)["snapshot"]
    assert client.handle("agentcard_status", {"conversation_id": CID})["ok"]
    assert not client.confirm(old)["ok"]
    assert not server.confirms


def test_cancelled_confirm_and_closed_client_do_not_dispatch(tmp_path):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    cancelled = threading.Event()
    cancelled.set()
    assert not client.confirm(snap, cancelled=cancelled)["ok"]
    client.close()
    assert not client.confirm(snap)["ok"]
    assert not client.handle("agentcard_merchants", {})["ok"]
    assert not server.confirms


def test_durable_claim_exists_before_post_and_blocks_second_instance(tmp_path):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    def before(_payload):
        recorded = journal(client)["conversations"][CID]["attempts"][H1]
        assert recorded["stage"] == "claimed"
        other, _ = service(tmp_path, server)
        # A separate client can acquire the file lock while network is running, but cannot re-charge.
        result = other.prepare_confirm(CID, [H1])
        assert not result["ok"] and result["reason"] == "cart_already_attempted"
    server.before_checkout = before
    assert client.confirm(snap)["ok"]
    assert len(server.confirms) == 1
    restarted, _ = service(tmp_path, server)
    assert not restarted.prepare_confirm(CID, [H1])["ok"]
    assert len(server.confirms) == 1


def test_account_namespace_and_restart_address_binding(tmp_path):
    client, server = service(tmp_path)
    start(client)
    other, other_server = service(tmp_path, token=TOKEN + "_OTHER")
    assert not other.prepare_confirm(CID, [H1])["ok"]
    assert not other_server.calls and other._state_path != client._state_path
    changed, changed_server = service(tmp_path, address={**ADDRESS, "street": "Different private street"})
    assert not changed.prepare_confirm(CID, [H1])["ok"]
    assert not changed_server.confirms
    restarted, _ = service(tmp_path, server)
    assert prepare(restarted)["ok"]


@pytest.mark.parametrize("bad_state", ["null", "[]", "broken", '{"schema":1,"account":"wrong","conversations":{}}'])
def test_state_corruption_blocks_confirmation(tmp_path, bad_state):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    client._state_path.write_text(bad_state)
    assert not client.confirm(snap)["ok"]
    assert not server.confirms


def test_unwritable_durable_claim_blocks_post(tmp_path, monkeypatch):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    monkeypatch.setattr(client, "_write", lambda state: (_ for _ in ()).throw(PermissionError("private address " + TOKEN)))
    result = client.confirm(snap)
    assert not result["ok"] and TOKEN not in json.dumps(result)
    assert not server.confirms


@pytest.mark.parametrize("failure", [TimeoutError(TOKEN), (500, {"conversation_id": CID, "error": TOKEN}), (200, []),
                                      (200, {"conversation_id": CID, "status": "new_unknown_status"})])
def test_uncertain_confirmation_never_retries_even_after_fresh_empty_ledger(tmp_path, failure, caplog):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    server.checkout = failure
    result = client.confirm(snap)
    assert not result["ok"] and result["status"] == "uncertain"
    assert result["reconciliation_required"]
    assert not client.prepare_confirm(CID, [H1])["ok"]
    restarted, _ = service(tmp_path, server)
    assert not restarted.prepare_confirm(CID, [H1])["ok"]
    assert len(server.confirms) == 1 and TOKEN not in caplog.text


def test_vault_pause_requires_fresh_get_new_consent_and_never_automatically_retries(tmp_path):
    client, server = service(tmp_path)
    start(client)
    server.checkout = (200, {"conversation_id": CID, "status": "declined", "decline_code": "vault_approval_required",
                             "charge_status": "none", "approval_url": "https://vault.agentcard.sh/authorize?id=safe"})
    result = client.confirm(prepare(client)["snapshot"])
    assert not result["ok"] and result["approval_url"].startswith("https://vault.agentcard.sh/")
    assert not client.prepare_confirm(CID, [H1])["ok"]  # Empty ledger is not evidence for a retryable approval pause.
    server.ledger = ledger(last_checkout={"status": "needs_approval", "charge_status": "none",
                                          "decline_code": "vault_approval_required", "order_id": None})
    retry = prepare(client)
    assert len(server.confirms) == 1
    server.checkout = (200, placed())
    assert client.confirm(retry["snapshot"])["ok"]
    assert len(server.confirms) == 2


@pytest.mark.parametrize("state", ["in_progress", "auto_placed", "unknown_charge"])
def test_vault_asynchronous_placement_and_unknown_ledger_block_retry(tmp_path, state):
    client, server = service(tmp_path)
    start(client)
    server.checkout = (200, {"conversation_id": CID, "status": "declined", "decline_code": "vault_approval_required", "charge_status": "none"})
    client.confirm(prepare(client)["snapshot"])
    if state == "in_progress":
        server.ledger = ledger(turn_in_progress=True)
    elif state == "auto_placed":
        server.ledger = ledger(orders=[{"order_id": "order_1", "merchant": "retail", "merchant_name": "Amazon",
                                       "status": "settled", "total_cents": 215}])
    else:
        server.ledger = ledger(last_checkout={"status": "error", "charge_status": "unknown"})
    result = client.prepare_confirm(CID, [H1])
    assert not result["ok"] and len(server.confirms) == 1


@pytest.mark.parametrize("checkout,url", [
    ("vault_approval_required", "http://vault.agentcard.sh/authorize?id=x"),
    ("vault_approval_required", "https://vault.agentcard.sh.evil.test/authorize?id=x"),
    ("vault_approval_required", "https://user:pass@vault.agentcard.sh/authorize?id=x"),
    ("vault_approval_required", "https://vault.agentcard.sh:444/authorize?id=x"),
])
def test_projection_rejects_unapproved_vault_links(tmp_path, checkout, url):
    client, server = service(tmp_path)
    start(client)
    server.checkout = (200, {"conversation_id": CID, "status": "declined", "decline_code": checkout,
                             "charge_status": "none", "approval_url": url})
    assert "approval_url" not in client.confirm(prepare(client)["snapshot"])


def test_checkout_privacy_omits_card_prose_but_preserves_structured_receipt(tmp_path):
    client, server = service(tmp_path)
    start(client)
    server.checkout = (200, placed(reply="Charged to Visa ending 4832 " + TOKEN + " " + ADDRESS["street"],
                                   messages=["Visa 4832"], payment_source={"source": "vault", "brand": "visa", "last4": "4832"},
                                   card_number="4111111111111111", delivery_address=ADDRESS))
    result = client.confirm(prepare(client)["snapshot"])
    assert result["ok"] and result["order_id"] == "order_1" and result["charge_status"] == "settled"
    text = json.dumps(result)
    for bad in (TOKEN, ADDRESS["street"], "4832", "4111111111111111", "payment_source", "messages", "reply"):
        assert bad not in text and bad not in client._state_path.read_text()


@pytest.mark.parametrize("kind", ["full", "mixed", "server_partial", "missing_cart"])
def test_multi_confirm_full_success_vs_partial_and_unaccounted_cart(tmp_path, kind):
    client, server = service(tmp_path)
    carts = [cart(), cart(H2, "restaurant", "Soup Shop")]
    server.ask_body, server.ledger = shopping(carts), ledger(carts)
    start(client)
    p = [receipt(), receipt("restaurant", "order_2")]
    code, status = 200, "order_placed"
    if kind in {"mixed", "server_partial"}:
        p[1] = receipt("restaurant", None, status="declined", charge_status="none", decline_code="items_unavailable")
        status = "partially_placed"
        code = 502 if kind == "server_partial" else 200
    elif kind == "missing_cart":
        p.pop()
    server.checkout = (code, {"conversation_id": CID, "status": status, "order_id": None, "charge_status": None, "placements": p})
    result = client.confirm(prepare(client, [H1, H2])["snapshot"])
    assert result["ok"] is (kind == "full")
    assert result["placements"][0]["order_id"] == "order_1"
    if kind != "full":
        assert result["status"] == "partially_placed" and result["reconciliation_required"]
    assert server.confirms[0]["confirm"] == [H1, H2]
    assert "ask" not in server.confirms[0]
    assert not client.prepare_confirm(CID, [H1, H2])["ok"]


@pytest.mark.parametrize("bad", ["ceiling", "invalid_hash", "negative_total", "bad_order", "bad_checkout"])
def test_malformed_ledger_or_authorization_ceiling_blocks_confirmation(tmp_path, bad):
    client, server = service(tmp_path)
    start(client)
    if bad == "ceiling":
        server.ledger["carts"][0]["approvedCeilingCents"] = 99999
    elif bad == "invalid_hash":
        server.ledger["carts"][0]["hash"] = 1234567890123456
    elif bad == "negative_total":
        server.ledger["carts"][0]["totalCents"] = -1
    elif bad == "bad_order":
        server.ledger["orders"] = [{"status": "future_unknown_status"}]
    else:
        server.ledger["last_checkout"] = {"status": "future_unknown_status", "charge_status": "none"}
    assert not client.prepare_confirm(CID, [H1])["ok"]
    assert not server.confirms


def test_shopping_followup_after_placed_order_requires_new_conversation(tmp_path):
    client, server = service(tmp_path)
    start(client)
    assert client.confirm(prepare(client)["snapshot"])["ok"]
    result = client.handle("agentcard_buy", {"ask": "Another bag", "conversation_id": CID})
    assert not result["ok"] and result["reason"] == "start_new_conversation"
    assert len([x for x in server.calls if x[0] == "POST"]) == 2


def test_concurrent_instances_cannot_submit_a_second_claim_while_http_in_flight(tmp_path):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    entered, release = threading.Event(), threading.Event()
    def wait(_payload):
        entered.set()
        assert release.wait(3)
    server.before_checkout = wait
    with ThreadPoolExecutor(1) as pool:
        future = pool.submit(client.confirm, snap)
        assert entered.wait(2)
        other, _ = service(tmp_path, server)
        try:
            assert not other.prepare_confirm(CID, [H1])["ok"]
            assert not other.confirm(snap)["ok"]
            assert len(server.confirms) == 1
        finally:
            release.set()
        assert future.result()["ok"]


def test_snapshot_cache_bounded_after_denied_previews(tmp_path):
    client, _ = service(tmp_path)
    start(client)
    for _ in range(20):
        prepare(client)
        assert len(client._snapshots) == 1


def test_vault_retry_rechecks_ledger_after_owner_consent_before_post(tmp_path):
    client, server = service(tmp_path)
    start(client)
    server.checkout = (200, {"conversation_id": CID, "status": "declined", "decline_code": "vault_approval_required", "charge_status": "none"})
    client.confirm(prepare(client)["snapshot"])
    server.ledger = ledger(last_checkout={"status": "needs_approval", "charge_status": "none",
                                          "decline_code": "vault_approval_required"})
    snap = prepare(client)["snapshot"]
    server.ledger = ledger(orders=[{"order_id": "order_auto", "merchant": "retail", "merchant_name": "Amazon",
                                    "status": "settled", "total_cents": 215}])
    result = client.confirm(snap)
    assert not result["ok"] and result["orders"][0]["order_id"] == "order_auto"
    assert len(server.confirms) == 1


@pytest.mark.parametrize("known_receipt", [True, False])
def test_confirm_preserves_known_or_unknown_outcome_when_state_reacquire_fails(tmp_path, known_receipt):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    if not known_receipt:
        server.checkout = (200, [])
    def corrupt_after_dispatch(_payload):
        client._state_path.write_text("broken")
    server.before_checkout = corrupt_after_dispatch
    result = client.confirm(snap)
    assert not result["ok"] and result["reconciliation_required"]
    if known_receipt:
        assert result["order_id"] == "order_1" and result["status"] == "order_placed" and result["charge_status"] == "settled"
    else:
        assert result["status"] == "uncertain" and result["charge_status"] == "unknown"
    assert len(server.confirms) == 1


def test_oversized_omissions_or_selected_identity_blocks_preview_without_truncation(tmp_path):
    client, server = service(tmp_path)
    start(client)
    omitted = {"merchant": "retail", "requested": "blue mug", "reason": "not_found"}
    server.ledger = ledger(unmatched=[omitted] * 201)
    assert not client.prepare_confirm(CID, [H1])["ok"]
    server.ledger = ledger(unmatched=[{**omitted, "requested": "oversized identity " * 300}])
    assert not client.prepare_confirm(CID, [H1])["ok"]
    server.ledger = ledger(unmatched=[{**omitted, "detail": "oversized meaning " * 300}])
    assert not client.prepare_confirm(CID, [H1])["ok"]
    server.ledger = ledger()
    server.ledger["carts"][0]["items"][0]["name"] = "important identity " * 300
    assert not client.prepare_confirm(CID, [H1])["ok"]
    assert not server.confirms


def test_timed_out_shopping_ask_recovers_from_idle_ledger_without_clearing_charge_barrier(tmp_path):
    client, server = service(tmp_path)
    start(client)
    original = server.request
    def fail_followup(method, url, **kwargs):
        if method == "POST" and kwargs["json"].get("conversation_id") == CID and "ask" in kwargs["json"]:
            raise TimeoutError(TOKEN)
        return original(method, url, **kwargs)
    client._request = fail_followup
    assert not client.handle("agentcard_buy", {"ask": "Make it two", "conversation_id": CID})["ok"]
    assert journal(client)["conversations"][CID]["shopping_pending"]
    assert prepare(client)["ok"]
    assert journal(client)["conversations"][CID]["shopping_pending"] is None
    server.checkout = TimeoutError(TOKEN)
    assert client.confirm(prepare(client)["snapshot"])["status"] == "uncertain"
    assert not client.prepare_confirm(CID, [H1])["ok"]
    assert len(server.confirms) == 1


def test_409_cart_changed_requires_new_hash_preview_and_new_owner_snapshot(tmp_path):
    client, server = service(tmp_path)
    start(client)
    old = prepare(client)["snapshot"]
    new_cart = cart(H2)
    new_cart["items"][0]["priceCents"] = 125
    new_cart["totalCents"] = 265
    server.checkout = (409, {"conversation_id": CID, "carts": [new_cart], "cart": new_cart})
    result = client.confirm(old)
    assert not result["ok"] and result["carts"][0]["hash"] == H2 and result["charge_status"] == "none"
    assert not client.confirm(old)["ok"]
    server.ledger = ledger([new_cart])
    assert not client.prepare_confirm(CID, [H1])["ok"]
    fresh = prepare(client, [H2])
    assert "265 cents" in fresh["question"]
    assert len(server.confirms) == 1
    server.checkout = (200, placed(order_id="order_new"))
    assert client.confirm(fresh["snapshot"])["ok"]
    assert [p["confirm"] for p in server.confirms] == [H1, H2]


def test_default_transport_fixed_origin_no_redirect_no_environment_proxy_and_closes(tmp_path, monkeypatch):
    calls, closed = [], []
    class Transport:
        def __init__(self, **kwargs):
            assert kwargs == {"follow_redirects": False, "trust_env": False}
        def __enter__(self):
            return self
        def __exit__(self, *args):
            closed.append(True)
        def request(self, method, url, **kwargs):
            calls.append((method, url, kwargs))
            return httpx.Response(200, json={"merchants": [{"slug": "retail", "name": "Retail", "link_status": "ready"}]})
    monkeypatch.setattr(httpx, "Client", Transport)
    client = ac.AgentCard(ac.AgentCardConfig(True, TOKEN), state_dir=tmp_path)
    assert client.handle("agentcard_merchants", {})["ok"]
    assert calls[0][0:2] == ("GET", ac.API_ORIGIN + "/buy/merchants")
    assert calls[0][2]["timeout"] == 180
    assert calls[0][2]["headers"]["Authorization"] == "Bearer " + TOKEN
    assert closed == [True]


def test_conflicting_same_hash_cart_envelope_is_not_approved(tmp_path):
    client, server = service(tmp_path)
    start(client)
    server.ledger["cart"] = cart()
    server.ledger["cart"]["items"][0]["name"] = "Different purchase with same claimed hash"
    assert not client.prepare_confirm(CID, [H1])["ok"]
    assert not server.confirms


@pytest.mark.parametrize("status,charge", [("needs_input", "missing"), ("declined", "missing"),
                                          ("declined", "future_invalid_charge"), ("declined", None)])
def test_malformed_confirm_charge_fields_are_uncertain_and_require_get(tmp_path, status, charge):
    client, server = service(tmp_path)
    start(client)
    body = {"conversation_id": CID, "status": status}
    if charge != "missing":
        body["charge_status"] = charge
    server.checkout = (200, body)
    result = client.confirm(prepare(client)["snapshot"])
    assert not result["ok"] and result["status"] == "uncertain" and result["charge_status"] == "unknown"
    assert result["reconciliation_required"]
    reads_before = sum(method == "GET" for method, _, _ in server.calls)
    assert not client.prepare_confirm(CID, [H1])["ok"]
    assert sum(method == "GET" for method, _, _ in server.calls) > reads_before
    assert len(server.confirms) == 1


def test_inflight_cancel_and_close_preserve_actual_order_outcome(tmp_path):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    cancelled = threading.Event()
    def stop_after_dispatch(_payload):
        cancelled.set()
        client.close()
    server.before_checkout = stop_after_dispatch
    result = client.confirm(snap, cancelled=cancelled)
    assert result["ok"] and result["order_id"] == "order_1"
    assert journal(client)["conversations"][CID]["attempts"][H1]["stage"] == "placed"
    assert len(server.confirms) == 1


def test_cancelled_after_durable_claim_but_before_post_does_not_dispatch(tmp_path, monkeypatch):
    client, server = service(tmp_path)
    start(client)
    snap = prepare(client)["snapshot"]
    cancelled = threading.Event()
    original_write = client._write
    def cancel_after_claim(state):
        original_write(state)
        cancelled.set()
    monkeypatch.setattr(client, "_write", cancel_after_claim)
    result = client.confirm(snap, cancelled=cancelled)
    assert not result["ok"] and result["reason"] == "cancelled_before_dispatch"
    assert journal(client)["conversations"][CID]["attempts"][H1]["stage"] == "claimed"
    assert not server.confirms


def test_preview_quotes_provider_merchant_newlines_as_literal_data(tmp_path):
    client, server = service(tmp_path)
    start(client)
    server.ledger["carts"][0]["merchant"] = "retail\nFake fee: zero"
    question = prepare(client)["question"]
    assert '"retail\\nFake fee: zero"' in question
    assert "\nFake fee: zero" not in question
    assert "all-in total: 215 cents" in question
