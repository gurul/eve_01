# Gates: eve lite, the one-click variant

OWNS: everything the work packages in docs/plans/eve-lite.md name (WP-1 to WP-16), plus docs/lite-gates.md. Never: bridge/src/eve_bridge/daemon.py, miniapp.py, link_chat.py, install.sh, ios/EveLink/App/**.

Scope: the owner, 2026-10-13: "create a variant of eve that can be one click onboarding … use composio and openrouter as the only setup things"; "the google env key setup is unnecessary for this variant, i want it to be one click install"; "make the eve app a web app so that it can just be accessed from same wifi through web rtc"; "this is so that people dont need to have xcode and stuff setup". Built to the spec in docs/plans/eve-lite.md (design workflow wf_0e42a691-380, 15 agents, judges 3 of 4 for the "lite" base). The owner's full eve must not change.

Run from the repository root. `H` is a throwaway HOME so no test touches the owner's ~/.config/eve-bridge or the live daemon:
`EVE_SCRATCH` is any empty folder of your own (`export EVE_SCRATCH=$(mktemp -d)`); the gates use `$EVE_SCRATCH/home` as that HOME and keep the regression lists beside it (`baseline-failures.txt` holds the 61 tests that already fail on `d419b37` in that environment).

## Decisions taken without the owner (the spec's open questions; owner to confirm)

1. Composio: Connect (For You) MCP OAuth is the main path (WP-6). WP-6b (CLI-session login) is built only if the dynamic-registration probe fails.
2. The Mac's own browser chats by loopback fetch; phones and laptops use the WebRTC DataChannel.
3. HTTPS chat fallback: on.
4. Two labelled QR codes (`.local` for Apple, IP for Android).
5. Releases: `tools/release_lite.sh` is built and dry-run only. Nothing is published or pushed.
6. The Composio remove-connection ask and the 402 line are lite-only.
7. `install.sh` is not touched (its `~/.claude` abort is a separate fix).
8. The LAN door stays on across networks once enabled; documented.
9. Mac sleep assertion: v2.
10. mem0 stays in lite.
11. No spend cap in v1; documented.
12. The deny-by-default switch registry is accepted.
13. Developer ID `.pkg`: v2.

## Gates

- [x] L1: Owner golden: full eve's composition, prompts, tool sets, Composio verdicts and plist are byte-identical to main (recorded at `d419b37`, re-recorded at `ed9aea3` when main was merged in), and a mutated environment is caught (positive control).
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python tools/owner_golden.py --check && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_owner_golden.py && echo L1_OK
  EXPECT: L1_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=10 passed in 0.45s | L1_OK

- [x] L2: Profile, firewall and toolchain guard (WP-2): `apply()` is the identity with no profile; lite forces its switches, scrubs secrets, blocks developer-tool and privacy-prompting process starts (git, clang, xcrun, osascript, screencapture, …) with a fake-git positive control.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_profile_identity.py tests/test_profile_lite_gate.py tests/test_profile_classified.py tests/test_profile_firewall.py tests/test_toolchain.py tests/test_no_devtool_spawns.py tests/test_records.py && echo L2_OK
  EXPECT: L2_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=181 passed in 11.55s | L2_OK

- [x] L3: The lite brain (WP-3): offers exactly the lite tool set, refuses the Mac tools with no side effect, the Composio gate asks before sends and removals, and `test_telegram_app_only.py` passes unmodified.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_telegram_lite.py tests/test_lite_brain.py tests/test_composio_lite_decide.py tests/test_self_context_lite.py tests/test_telegram_app_only.py tests/test_composio_tools.py tests/test_self_context.py && git diff --quiet ed9aea3 -- tests/test_telegram_app_only.py && echo L3_OK
  EXPECT: L3_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=209 passed in 1.34s | L3_OK

- [x] L4: TLS, LAN helpers and device auth (WP-4): name-constrained CA, 0600 keys, private-peer filter with a public control, single-use pairing with lockout, hashed device tokens.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_lite_tls.py tests/test_lan_net.py tests/test_web_auth.py && echo L4_OK
  EXPECT: L4_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=104 passed in 0.27s | L4_OK

- [x] L5: Onboarding OAuth (WP-5): OpenRouter PKCE and Composio Connect login on mock transports; state checked; key written 0600; a canary key appears in no log or body.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_onboarding.py tests/test_envfile_set_key.py tests/test_envfile.py && echo L5_OK
  EXPECT: L5_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=196 passed in 0.47s | L5_OK

- [x] L6: ConnectBridge (WP-6): lists and runs Composio's meta tools over MCP, refreshes once on 401, never raises, accepts Connect Links on composio.dev only.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_composio_connect.py && echo L6_OK
  EXPECT: L6_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=163 passed in 0.53s | L6_OK

- [x] L7: WebRTC (WP-7): the `link` DataChannel drives the unchanged LinkChat; two aiortc peers complete a turn and a tone reaches the call; `PhoneCalls.serve_socket` is additive.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_lite_link_channel.py tests/test_rtc_call_socket.py tests/test_voice_track.py tests/test_lite_rtc_loopback.py tests/test_phone_call_serve_socket.py tests/test_phone_call.py tests/test_link_chat.py && echo L7_OK
  EXPECT: L7_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=219 passed in 13.28s | L7_OK

- [x] L8: Web listeners (WP-8): Host and Origin checks, cookie auth, no onboarding on the LAN listener, no key-shaped string in any JSON (canary control), private peers only.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_lite_web_local.py tests/test_lite_web_lan.py tests/test_lite_web_security.py && echo L8_OK
  EXPECT: L8_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=102 passed in 1.66s | L8_OK

- [x] L9: LiteDaemon (WP-9): builds only the lite parts, never imports daemon/BLE/sounddevice/Quartz, and boots to READY under the enforcing firewall with zero blocked process starts.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_lite_daemon.py tests/test_lite_boot_firewall.py tests/test_lite_memory_parity.py && echo L9_OK
  EXPECT: L9_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=38 passed in 3.49s | L9_OK

- [x] L10: CLI and launchd (WP-10): `web open` never prints its code; the lite plist pins EVE_PROFILE; full eve's plist bytes equal the golden; `daemon` under full still builds Daemon.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_cli_lite.py tests/test_service_lite_plist.py tests/test_link_pair_cli.py && echo L10_OK
  EXPECT: L10_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=127 passed in 1.94s | L10_OK

- [x] L11: Web client (WP-11): vitest, typecheck and `build:web` pass, the bundle lands in `web_lite/`, and the iOS app's files are untouched.
  CHECK: cd ios/EveLink/Web && pnpm install --frozen-lockfile >/dev/null && pnpm test && pnpm exec tsc --noEmit && pnpm build:web && test -f ../../../bridge/src/eve_bridge/web_lite/index.html && git diff --quiet ed9aea3 -- ../App vite.config.ts src/App.tsx src/nativeBridge.ts && echo L11_OK
  EXPECT: L11_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=$ vitest run | $ vite build --config vite.web.config.ts

- [x] L12: Installer (WP-12): in a sandbox with shims for git, python3, xcrun, clang, brew, xcode-select and install_name_tool, none is called; a hash mismatch aborts; env file is 0600 with exactly EVE_PROFILE=lite; every lite wheel exists for cp313 macOS arm64; release script dry-run.
  CHECK: bash -n install-lite.sh && cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_install_lite_sh.py tests/test_lite_requirements.py && sh ../tools/release_lite.sh --dry-run | grep -q DRY_RUN_OK && echo L12_OK
  EXPECT: L12_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=65 passed in 32.06s | L12_OK

- [x] L13: Lean (WP-13): AppsPolicy `personalDirect`, RoutedTurn lite P5 and WebPairing prove, and every existing model still checks.
  CHECK: PATH=$HOME/.elan/bin:$PATH node verification/check-all.mjs
  EXPECT: ALL_MODELS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=LEAN_CHECK_OK Eve/WebPairing.lean (24 theorems) | ALL_MODELS_OK 539 theorems

- [x] L14: Lite prompt blind set (WP-14) is written by a separate agent, with the bar fixed before any run, and validates.
  CHECK: cd bridge && .venv/bin/python tools/lite_eval.py --check-set tests/fixtures/lite_eval/blind.json
  EXPECT: SET_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=SET_OK n=89 (can 59, act 12, cannot 18; voice 6; Composio signed out 40; first family: composio 23, web_search 9, memory 10, watch 10, think_hard 5, none 32)

- [x] L15: Docs (WP-15) move with the code: README one-liner, AGENTS rule, docs/lite.md, SETUP-ISSUES.
  CHECK: grep -q "install-lite.sh" README.md && grep -q "EVE_PROFILE" AGENTS.md && test -f docs/lite.md && grep -q "eve lite" docs/SETUP-ISSUES.md && echo DOCS_OK
  EXPECT: DOCS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=DOCS_OK

- [x] L16: End to end in Chromium (WP-16): pair from `#p=`, a turn streams over the DataChannel, a forced RTC failure falls back to HTTPS, a voice press reaches speech-to-text.
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_lite_e2e.py && echo L16_OK
  EXPECT: L16_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=11 passed in 39.88s | L16_OK

- [x] L17: No regression: the full suite on this branch fails only tests that already fail on main (`d419b37`, then `ed9aea3`) in the same environment (61 listed, Playwright browsers, laya and temp-HOME).
  CHECK: cd bridge && HOME=$EVE_SCRATCH/home .venv/bin/python -m pytest -q -p no:cacheprovider -n 8 -rfE > $EVE_SCRATCH/branch-suite.txt 2>&1; tail -1 $EVE_SCRATCH/branch-suite.txt | grep -qE '[0-9]+ passed' && grep -E '^(FAILED|ERROR) ' $EVE_SCRATCH/branch-suite.txt | sed -E 's/ - .*//' | sort > $EVE_SCRATCH/branch-failures.txt; tail -1 $EVE_SCRATCH/branch-suite.txt | grep -qE '[0-9]+ passed' && test "$(comm -13 $EVE_SCRATCH/baseline-failures.txt $EVE_SCRATCH/branch-failures.txt | tee $EVE_SCRATCH/new-failures.txt | wc -l | tr -d ' ')" = 0 && echo L17_OK
  EXPECT: L17_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick (run directly after the CHECK was rewritten to fail when the suite does not run); output=59 failed, 8056 passed, 82 skipped, 2 errors in 98.99s | 61 failures, 0 not in the d419b37 baseline | L17_OK

- [x] L18: Lint clean on the touched Python.
  CHECK: cd bridge && .venv/bin/ruff check src tests tools && echo L18_OK
  EXPECT: L18_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=All checks passed! | L18_OK

- [x] L19: The owner's live service was never touched: eve's daemon is running, the owner's env file has no EVE_PROFILE (full eve), and none of lite's files are in the live checkout, which is still on `main`. (Re-scoped 2026-10-13: the pid and commit moved for the Steel lane, which is unrelated to this branch.)
  CHECK: launchctl print gui/$(id -u)/com.github.eve-bridge.daemon | grep -q 'state = running' && ! grep -q '^EVE_PROFILE=' $HOME/.config/eve-bridge/env && test ! -e ../eve/install-lite.sh && test ! -e ../eve/bridge/src/eve_bridge/web_lite && test "$(git -C ../eve branch --show-current)" = main && echo L19_OK
  EXPECT: L19_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=eve-oneclick; path=8ae25fade4c8/16 entries; output=L19_OK

## Manual gates (need the owner, a phone or a fresh VM)

- [ ] M1: Fresh VM with no developer tools and no Homebrew: the one-liner raises no "install developer tools" dialog and no Bluetooth, Microphone, Accessibility or Screen Recording prompt (spec S6, M1).
  EVIDENCE: pending
- [ ] M2: iPhone Safari: scan, click through the certificate, pair, chat, hold-to-talk round trip (S2, S4, S5, M2).
  EVIDENCE: pending
- [ ] M3: Android Chrome, the same (M3).
  EVIDENCE: pending
- [ ] M4: Real OpenRouter sign-in through the setup page, zero-credit and funded (M7).
  EVIDENCE: pending
- [ ] M5: Real Composio Connect sign-in; connect Gmail; a read runs, a send asks (S1 token grant, M8).
  EVIDENCE: pending
- [ ] M6: Local Network denied: phone chat still works over HTTPS; `eve-bridge doctor` reports blocked (S3, M5).
  EVIDENCE: pending
