# The vault: passwords, cards and 2FA codes no model sees

eve can sign in, pay and enter 2FA codes for you without a model ever reading the
password, the card number or the code. The idea comes from
[OpenInstinct](https://github.com/Merit-Systems/OpenInstinct) (MIT): a model asks for
a vault item by an id, and code puts the value into the page.

What a model is told about an item is one line:

```text
v_3k9f2a7q login "Chase" for chase.com
v_8d1m0x4e card "Visa 4242"
v_q7w2e5r9 totp "Chase 2FA" for chase.com
```

The values stay in the macOS Keychain. Code reads them only at the moment it fills
a field, and they never go into a prompt, a log, a reply or a screenshot.

## Adding items

Values are typed at the terminal, never into chat:

```sh
eve-bridge vault add login --site chase.com --label Chase       # asks: Username, Password
eve-bridge vault add card --label "Visa 4242"                   # asks: Card number, Expiry, CVC, Name on card
eve-bridge vault add totp --site chase.com --label "Chase 2FA"  # asks: the 2FA secret or its otpauth:// link
eve-bridge vault list                                            # ids, kinds, labels, sites; never a value
eve-bridge vault remove v_3k9f2a7q
```

For a 2FA item, paste the secret shown under "can't scan the QR code?" when you
turn on an authenticator app, or the `otpauth://` link inside the QR code. eve then
generates the six-digit code itself (RFC 6238; SHA1, SHA256 and SHA512; 6 to 8
digits) each time a page asks for one.

## What each kind fills, and where

| Kind | Fills | Where it may fill |
|---|---|---|
| `login` | username and password | Only on its site: `chase.com` fills on `chase.com` and `secure.chase.com`, never on `chase.com.example` or another scheme or port. |
| `card` | number, expiry (`MM/YY`, or month and year selects), CVC, name on card | Any page, with no question. A purchase still needs your yes, because eve's gate stops at Pay, Place order and similar buttons. |
| `totp` | the current one-time code | Only on its site, like a login. |

Logins and 2FA codes check the site against the page's real URL, which comes from
the browser itself and never from a model. A model can't talk the vault into filling
another site.

## How the browser lane uses it

When eve plans a web task (`EVE_BROWSER_ATTACH=1` or `EVE_BROWSER_OWN=1`), the planner
sees the vault's lines and can write a `fill` step whose text is an id. The page then
finds the form's fields by what they are (`autocomplete`, `type`, label, name),
fills them with Playwright, and reports which parts it filled. It never reports the
values. Jev doesn't pick these fields.

Three more things keep the values out of models:

- The page reader never reads what is in a password, card or one-time-code field,
  or in any field the vault filled. A model sees the field's label only.
- The tab picture eve sends you paints those fields over in black.
- The ledger says `filled username, password from 'Chase'`.

## How Codex tasks use it: `vault_fill`

A Codex computer task gets eve's `eve_vault` MCP server whenever the vault holds an
item. It has two tools:

- `vault_list` gives the lines above.
- `vault_fill(id, part, page, field)` puts one part of an item (`password`, `number`,
  `code`, …) into the page.

Codex never sees the value, and the tool's answer says only what was filled.

`vault_fill` works through the macOS Accessibility API. It reads the browser's tree,
finds the field, and sets its value. The page gets the same `input` and `change`
events as from autofill. No key is pressed, the browser doesn't need to be in front,
and nothing else is touched. Two earlier designs failed a real Codex run:

- Typing where the Mac's keyboard focus was failed, because Codex's clicks never move
  the Mac's focus, so keys would go to the address bar.
- Using the field the page had focused failed, because Chrome in the background
  reports no focused field.

Which field it fills:

- **Page.** For a login or 2FA item, a page on its site. For a card, the page Codex
  names and the frames inside it. No other page's fields are read at all.
- **Password.** The page's password field. A password goes nowhere else.
- **Username.** A focused field, else a field called username or email, else the
  last text field before the password.
- **Card parts and codes.** A focused field, else the one field whose label fits.
- **Ambiguous cases.** Codex can name the field's label. Anything still ambiguous is
  refused, and the refusal lists the candidates' labels (never values).

Codex calls to `eve_vault`'s two tools are accepted without asking you. Any other
tool or server still goes through the usual permission questions.

## Where values live

| Place | What |
|---|---|
| Keychain, service `eve-vault`, one item per id | The values, as hex-encoded JSON |
| `~/.config/eve-bridge/vault.json` (mode 600) | Ids, kinds, labels and sites only |

eve drives the Keychain through `/usr/bin/security`. A value is never passed on a
command line, where other programs could see it in the process list: the add command
goes to `security -i` on stdin. Items created this way let `security` read them
without asking, which also means any program running as you can read them the same
way. That's the same exposure as eve's `env` file. `EVE_VAULT_KEYCHAIN` points the
vault at another keychain file, and `EVE_VAULT_INDEX` moves the index.

## Limits that remain

- **What a model can learn.** The id, kind, label and site of each item, and which
  fields were filled.
- **Codex screenshots.** Codex's own screenshots of the page can show a card number
  that a site displays unmasked. Password fields show dots. eve masks only its own
  browser lane's pictures.
- **Active tabs only.** Codex fills reach each window's active tab. A page in a
  background tab is invisible to the Accessibility tree, so keep the task's tab in
  front in its window.
- **Accessibility must be on.** Codex fills need the browser's web Accessibility tree.
  Chrome keeps it on while an assistive client such as Codex's computer use is
  reading it. Chrome for Testing needs `--force-renderer-accessibility`.
- **Main frame only in the browser lane.** It fills fields in the page's main frame.
  Through `vault_fill` (Codex), a card also fills a payment provider's frame inside
  the page Codex names; a live test fills a card field in a frame from another host.
  A login or 2FA code fills a frame only when the frame is on its own site.
- **No passkeys and no SMS codes.** The vault holds only what is listed above.
- **Clicking "Sign in" still asks.** eve's general gate stops before buttons such as
  "Sign in" in the browser lane and asks you first. The vault fills the form, and
  your yes submits it.

## Checks

The ledger is [vault-gates.md](vault-gates.md). Tests: `bridge/tests/test_vault.py`,
`test_vault_fill.py`, `test_vault_mcp.py`. Live checks: `EVE_LIVE=1` for the
Keychain, Chromium and Accessibility tests, `bridge/tools/vault_codex_check.py`
(a Codex thread lists `eve_vault`) and `bridge/tools/vault_codex_e2e.py` (a real
Codex task signs in to a local page; it opens a tab in your Chrome).
