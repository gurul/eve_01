# AgentCard purchases

AgentCard is an optional shopping integration for eve's shared chat and call
brain. Use it through Telegram, the Mac chat, Voice PE desk push-to-talk, a phone
call, or the Action Button through Eve Link. These surfaces share the same
owner approval path. The separate wake-word Live voice backend does not offer
the purchase tools.

The implementation follows AgentCard's [Purchase API guide](https://docs.agentcard.sh/vault/integrations/ecommerce-apis/purchase-api),
[buy reference](https://docs.agentcard.sh/api-reference/purchases/buy),
[conversation status reference](https://docs.agentcard.sh/api-reference/purchases/conversation),
and [merchant reference](https://docs.agentcard.sh/api-reference/purchases/merchants),
retrieved on 2026-10-03. Available merchants depend on your account and AgentCard's
current integrations; eve reads the live list instead of assuming a shop is
available.

## Setup

Set up the [Telegram door](stackchan/telegram.md) and its OpenRouter key first; the
shared chat brain needs both. Add these settings to
`~/.config/eve-bridge/env`, then restart the daemon:

```dotenv
EVE_AGENTCARD=1
AGENTCARD_ACCESS_TOKEN=your-user-scoped-token
```

The feature ships off. A token alone does not turn it on. Use a user connection
`access_token` or an organization-minted **user buy token**. An organization API
token itself cannot call `/buy`. Token issuance, merchant account linking and
Vault funding/authorization happen through AgentCard; eve does not provision
an organization or collect card details. Keep credentials in the private env file,
not in a chat or a shopping request. Restrict it to your user:

```bash
chmod 600 ~/.config/eve-bridge/env
```

For an explicit delivery address, create a separate private JSON file. The API
requires `street`, `city`, `state` and `zip`. Retail shipping also requires a phone
number. Other supported fields are `address2`, `name`, `country`, and
`can_leave_at_door`. For example, using placeholder data:

```json
{
  "street": "123 Example Street",
  "city": "Example City",
  "state": "CA",
  "zip": "94107",
  "country": "US",
  "phone": "+15555550123",
  "can_leave_at_door": false
}
```

```bash
chmod 600 ~/.config/eve-bridge/agentcard-address.json
```

Point the env file at it:

```dotenv
EVE_AGENTCARD_ADDRESS_FILE=/absolute/path/to/agentcard-address.json
EVE_AGENTCARD_TIMEOUT_SECS=180
```

The timeout defaults to 180 seconds and must be at least 120, as AgentCard's
merchant/cart loop can be slow. The address is bound from the first turn and
resent on every request in that conversation, including confirmation. After
changing the address configuration and restarting eve, start a new shopping
conversation. Eve blocks reuse when the address binding cannot be recovered
safely. With no explicit file, AgentCard uses its account-side address behavior.
The journal is scoped to a fingerprint of the configured token. After rotating
credentials, existing provider conversations remain readable through status,
but checkout must begin in a new local shopping conversation. Token refresh and
OAuth sign-in are managed outside eve.

## Shopping and checking an order

Ask eve, for example, “Which shops can I use through AgentCard?” or “Find a pack
of printer paper; show me the cart first.” Four tools are available when enabled:

| Tool | Purpose |
|---|---|
| `agentcard_merchants` | Current merchant list, linking state and capabilities |
| `agentcard_buy` | Start or continue a shopping conversation using `ask` and an optional `conversation_id`; this path never sends a confirmation |
| `agentcard_status` | Read a conversation's current carts, checkout state and order ledger |
| `agentcard_confirm` | Prepare an exact selection of cart hashes, ask the owner, and submit once only after approval |

Eve branches on structured statuses and fields, rather than interpreting a
merchant's prose as a receipt. Catalog product identifiers and provided image
URLs help it resolve alternatives; `unmatched` reports requested items that are
not in the cart. Multiple carts can be selected together, and the approval must
show every selected cart. Fee and tip fields are shown separately; `totalCents`
is the API's all-in cart total. A new or changed cart requires a new preview and
approval.

Start a new shopping conversation after an order or partial placement. Eve
conservatively blocks further checkout in a conversation that already has
orders. A cart with a non-null `approvedCeilingCents` is also refused by this
integration; it does not authorize an undisclosed ceiling above the shown total.

Confirmation always requests `payment_source: "vault"`. Vault may require an
additional passkey approval in the owner's browser. Eve returns AgentCard's
validated HTTPS approval link. Ask eve to read conversation status after approval:
AgentCard may finish the order itself. If another submission is required, eve
asks for fresh approval; it does not automatically repeat the confirmation.

`order_placed` means an order was placed, not delivered. Charge state can still
be `confirming`; a partial placement or decline remains partial or declined.
Order status here comes from the conversation ledger. Separate retail shipment
tracking and cancellation/refunds are outside this integration.

If checkout times out, returns malformed data, loses its connection or has an
uncertain charge, eve records the attempt and directs the next step to status.
It does not blindly resubmit. A restart retains this protection. Do not delete
the journal to force a retry: inspect the AgentCard conversation or Vault order
first. An unavailable or corrupt journal blocks submission.

## Data and approvals

Each purchase needs a firm Yes/No answer to eve's code-composed cart preview.
The model cannot provide an `approved` flag, choose a payment credential, or
approve its own tool call. Typed answers, inline buttons and a bare spoken yes
all use the owner's existing question path. An ambiguous “yes, but change it”
does not approve a purchase. Old buttons and answers to older spoken questions
cannot authorize a new cart. Calls read the whole purchase preview in short pieces
so a long selection is not clipped by the normal speech length limit.

Type `stop` to withdraw queued or waiting purchase approval. Once a confirmation
has been sent, Stop cannot undo it: eve reports that it may already be in
flight and uses status to reconcile. Shutdown also withdraws pending approvals.
Ending a call releases its audio listener; a pending approval can remain visible
in Telegram and be answered there until it expires. Hanging up does not approve
or cancel an already submitted purchase.

Shopping asks and any explicit delivery address go to `https://api.agentcard.sh`
and AgentCard's integrations. Cart/product/order summaries reach eve's model,
and ordinary chat capture can retain them when memory is on. The client
allowlists result fields, omits payment-card metadata and redacts the configured
token and private address text from prose while retaining structured identifiers.
It supplies a safe approval summary to
history instead of saving the displayed preview verbatim. Text you independently
type into chat still follows the normal model and memory path; do not paste
credentials, addresses or card numbers there.

The purchase journal lives in `~/.config/eve-bridge/agentcard/` with directory
mode 700 and state-file mode 600. It stores minimal conversation and cart attempt
identities/outcomes, not the access token, delivery address or raw provider
responses. It remains necessary when conversation memory is off. Purchase
charges and merchant fees are separate from eve's model API spend ledger.

## Development checks

The regression tests use fake Telegram/model services and mocked HTTP responses;
they make no live purchase or financial API request:

```bash
cd bridge
.venv/bin/python -m pytest -q tests/test_agentcard.py tests/test_agentcard_tools.py tests/test_telegram_agentcard.py tests/test_phone_call_approval.py
```

Coverage includes exact cart binding, single-use attempts, ambiguous checkout
outcomes, private result projections, owner consent, Stop/shutdown, and spoken
answer routing. A real account checkout requires your credentials and a separate
owner-approved cart; it was not performed as part of developing this feature.
