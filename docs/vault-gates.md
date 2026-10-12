# Gates: eve's vault, secrets that never reach a model

OWNS: bridge/src/eve_bridge/vault.py (new), bridge/src/eve_bridge/vault_native.py (new), bridge/src/eve_bridge/vault_mcp.py (new), bridge/src/eve_bridge/browser_lane.py, bridge/src/eve_bridge/plan_contract.py, bridge/src/eve_bridge/plan_executor.py, bridge/src/eve_bridge/computer_agent.py (the planner call only), bridge/src/eve_bridge/codex_computer.py (thread config, instructions, vault approvals), bridge/src/eve_bridge/cli.py (the vault words), bridge/tests/test_vault.py (new), bridge/tests/test_vault_mcp.py (new), bridge/tests/test_vault_fill.py (new), bridge/tests/test_browser_lane.py, bridge/tools/vault_codex_check.py (new), docs/vault.md (new), docs/vault-gates.md, README.md, docs/stackchan/routing.md, docs/codex-computer-use/README.md

Scope: the owner, 2026-10-12, after comparing eve with OpenInstinct: build its vault idea (opaque handles; the
code fills the page; the model never sees a password, card or code) "and also fix limits, i dont want to be
limited": vault fill in Codex tasks as well as the Chrome lane, 2FA secrets stored and their codes generated, and
card fills with no per-fill yes (the existing gate still stops at Pay / Place order). Logins and 2FA codes fill
only on the site they were saved for. Values live in the macOS Keychain; a 0600 index holds handles, kinds,
labels and sites only. The leak fix that came first (the page collector no longer reads secret fields) is V0.

- [x] V0: The page collector never reads a secret field: a live page's password, shown password, card, CVC and one-time-code values appear nowhere in the snapshot or outline, an ordinary field's value still does, and the old collector fails the same test (checked by stash, 2026-10-12: the unlabelled password came back as its label).
  CHECK: cd bridge && EVE_LIVE=1 .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_browser_lane.py -k secret_field && echo V0_OK
  EXPECT: V0_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=1 passed, 11 deselected in 2.30s | V0_OK

- [x] V1: The vault's code passes its tests: RFC 6238 TOTP vectors (SHA1, SHA256, SHA512) and otpauth:// parsing; site matching (the saved host and its subdomains, scheme and port equal; a lookalike, a parent domain, another scheme or port refused); the Keychain wrapper sends a secret only on stdin, never on a command line; the 0600 index holds no secret; add, list, remove, and the `eve-bridge vault` words with secrets read by getpass.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_vault.py && echo V1_OK
  EXPECT: V1_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=8 passed, 1 skipped in 0.03s | V1_OK

- [x] V2: Live Keychain round trip in a throwaway keychain file (never the login keychain): add a login, a card and a 2FA secret, read each back exactly, remove them, and nothing is left.
  CHECK: cd bridge && EVE_LIVE=1 .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_vault.py -k live_keychain && echo V2_OK
  EXPECT: V2_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=1 passed, 8 deselected in 0.23s | V2_OK

- [x] V3: The browser lane's `fill` step passes its tests on fakes: the plan contract accepts `fill` only with a handle-shaped text and the planner is told the vault's handles, labels and sites (never a value); the executor runs a fill through the page and reports refusals; an effector without a vault refuses; a login handle on another site fills nothing.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_vault_fill.py -m "not live" && echo V3_OK
  EXPECT: V3_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=4 passed, 1 deselected in 0.04s | V3_OK

- [x] V4: Live in a real Chromium: a plan with fill steps fills an unlabelled login form, a card form (number, expiry, CVC, name) and a one-time-code field with the exact values (compared inside the page, which returns only true or false); a login handle saved for another site fills nothing; and the canary values appear nowhere in the planner's request, the snapshot, the outline, the ledger, the log lines or the result. The screenshot paints the secret fields over (the mask colour across the field's box), and the unmasked screenshot of the same field does not (positive control).
  CHECK: cd bridge && EVE_LIVE=1 .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_vault_fill.py -m live && echo V4_OK
  EXPECT: V4_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=1 passed, 4 deselected in 2.92s | V4_OK

- [x] V5: The vault MCP server passes its tests: initialize, tools/list (vault_list, vault_fill), vault_list returns handles, kinds, labels and sites and no value; vault_fill with a fake Mac finds the field by what it is on the item's own page (the password field; the username by focus, name or place before the password; card parts and codes by label), never walks a page on another site, narrows by Codex's page and field label, and refuses another site, a password into a non-password field, two pages or two fields it cannot tell apart, and a card with no page; no response ever holds a value. (Design changed 2026-10-12 after V11 failed twice: typing at the Mac's focus, then reading the page's focus, both failed with a real Codex task, see vault_native.py.)
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_vault_mcp.py -m "not live" && echo V5_OK
  EXPECT: V5_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=6 passed, 2 deselected in 0.06s | V5_OK

- [x] V6: Live native fill on this Mac: a headed Chromium behind Finder, nothing focused, two windows (the item's site and the same form on another host); vault_fill sets the username and the password exactly into the item's page (checked inside the page, with its input and change events fired), leaves the other host's form empty, refuses another site's login and a password into the Email field, and the browser never comes to the front.
  CHECK: cd bridge && EVE_LIVE=1 .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_vault_mcp.py -m live && echo V6_OK
  EXPECT: V6_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=2 passed, 6 deselected in 6.71s | V6_OK

- [x] V7: Codex tasks get the vault: unit tests show thread/start carries the eve_vault MCP server and the vault paragraph when the vault holds items (and neither when it is empty), and that only eve_vault's own tool approvals are accepted without the owner; live, a real Codex app-server thread lists eve_vault with vault_list and vault_fill.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_vault_mcp.py -k codex && .venv/bin/python tools/vault_codex_check.py
  EXPECT: VAULT_CODEX_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=cua_repl: present | VAULT_CODEX_OK

- [x] V8: The full bridge suite and lint pass, except the 11 tests that already fail on main before this work (test_computer_protocol_eval.py and test_computer_swarm_verify.py: a missing docs/computer-protocol-eval/ax.json and an empty error message; checked 2026-10-12 on unchanged main).
  CHECK: cd bridge && (.venv/bin/python -m pytest -q -p no:cacheprovider -n auto tests 2>&1 | .venv/bin/python -c "import re,sys; out=sys.stdin.read(); fails=set(re.findall(r'^FAILED (\S+?)(?:\[.*?\])?(?: - .*)?$', out, re.M)); known={'tests/test_computer_protocol_eval.py::test_existing_historical_reports_still_check_without_rewriting_them','tests/test_computer_protocol_eval.py::test_visual_checker_rejects_invalid_or_self_graded_evidence','tests/test_computer_protocol_eval.py::test_plot_checker_rejects_stale_pixels_even_when_summary_and_dimensions_match','tests/test_computer_swarm_verify.py::test_no_improvement_fails_and_graded_complete_positive_control_passes'}; m=re.search(r'(\d+) passed', out); new=fails-known; print('new failures:', sorted(new)) if new else None; ok=m and int(m.group(1))>7000 and not new and 'error' not in out.splitlines()[-1]; print('SUITE_OK' if ok else 'SUITE_BAD: '+out.splitlines()[-1]); sys.exit(0 if ok else 1)") && uv run ruff check src tests tools && echo V8_OK
  EXPECT: V8_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=All checks passed! | V8_OK

- [x] V9: The docs describe it: docs/vault.md (setup, what each kind fills, what the models see, where values live, the limits that remain), and README, routing and the Codex guide link to it.
  CHECK: python3 -c "import pathlib,sys; r=pathlib.Path('.'); v=(r/'docs/vault.md').read_text(); need=['eve-bridge vault add','Keychain','vault_fill','one-time','Only on its site']; miss=[w for w in need if w not in v]; links=[p for p in ('README.md','docs/stackchan/routing.md','docs/codex-computer-use/README.md') if 'vault.md' not in (r/p).read_text()]; print(miss, links) if miss or links else print('V9_OK'); sys.exit(1 if miss or links else 0)"
  EXPECT: V9_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=V9_OK

- [x] V10: No personal data or token-shaped strings in the diff (the repo is public).
  CHECK: (git diff HEAD; git ls-files -o --exclude-standard -z | xargs -0 cat) | grep -v 'CHECK: (git diff' | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'sk-[A-Za-z0-9]{20,}', r'/Users/') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=DIFF_CLEAN

- [x] V11: End to end through Codex: a real Codex computer task signs in to a local test page with a vault item in a throwaway keychain, the page's server receives exactly the saved username and password, and the password appears in none of Codex's events or its answer. Opens a tab in the owner's Chrome.
  CHECK: cd bridge && .venv/bin/python tools/vault_codex_e2e.py
  EXPECT: VAULT_E2E_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo root>; path=8ae25fade4c8/16 entries; output=canary in any event or the answer: False | VAULT_E2E_OK
