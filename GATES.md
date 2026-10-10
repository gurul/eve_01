# Gates: the rename to eve

OWNS: every tracked file and path (vendor/ excepted), ~/.config/eve-bridge, the launchd agent, the iOS app on the phone, ../portfolio/app/projects/page.js

Scope: the owner, 2026-10-10: "change name of repo and everywhere including app, phone is connected, to eve", then "Everything, including bundle ID", "also in portfolio", "push everything after". Then: "move everything into a brand new repo so that it looks new" — one commit, authored by the owner alone. The ledger this replaces (the chief's replanner) is in the previous repository at 97ea560.

- [ ] G1: No tracked text outside vendor/ says the old name, except the name of the upstream project it was forked from (tools/rename_check.py also proves it catches a planted positive).
  CHECK: python3 tools/rename_check.py
  EXPECT: RENAME_CLEAN
  EVIDENCE: pending

- [ ] G2: The bridge's full test suite passes.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && echo BRIDGE_TESTS_OK
  EXPECT: BRIDGE_TESTS_OK
  EVIDENCE: pending

- [ ] G3: Lint is clean (ruff is the project's linter; no type-checker is configured for Python).
  CHECK: cd bridge && uv run ruff check src tests tools && echo LINT_OK
  EXPECT: LINT_OK
  EVIDENCE: pending

- [ ] G4: The Lean models still build and check under their new module name.
  CHECK: cd verification && PATH=$HOME/.elan/bin:$PATH lake build && node check-all.mjs
  EXPECT: ALL_MODELS_OK
  EVIDENCE: pending

- [ ] G5: The iOS app builds for the simulator as EveLink, bundle com.github.eve-bridge.EveLink.
  CHECK: cd ios/EveLink && xcodegen generate -q && xcodebuild -project EveLink.xcodeproj -scheme EveLink -sdk iphonesimulator -configuration Debug -derivedDataPath build-sim build -quiet && /usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' build-sim/Build/Products/Debug-iphonesimulator/EveLink.app/Info.plist
  EXPECT: com.github.eve-bridge.EveLink
  EVIDENCE: pending

- [ ] G6: The daemon runs under com.github.eve-bridge.daemon from the eve_bridge package, and the old agent is gone.
  CHECK: launchctl print gui/$(id -u)/com.github.eve-bridge.daemon | grep -q 'state = running' && ! launchctl print gui/$(id -u)/com.github.cc-bu$(echo dd)y-bridge.daemon >/dev/null 2>&1 && pgrep -f 'eve_bridge.cli daemon' >/dev/null && echo DAEMON_OK
  EXPECT: DAEMON_OK
  EVIDENCE: pending

- [ ] G7: The portfolio lists eve, linking the renamed repo.
  CHECK: grep -q 'github.com/gurul/eve"' ../portfolio/app/projects/page.js && ! grep -qiE 'bu(dd)y' ../portfolio/app/projects/page.js && echo PORTFOLIO_OK
  EXPECT: PORTFOLIO_OK
  EVIDENCE: pending

- [ ] G8: GitHub has a new gurul/eve whose main is this repo's head, with one contributor: the owner.
  CHECK: test "$(gh api repos/gurul/eve --jq .name)" = eve && test "$(git ls-remote https://github.com/gurul/eve.git refs/heads/main | cut -f1)" = "$(git rev-parse HEAD)" && test "$(gh api repos/gurul/eve/contributors --jq '[.[].login]|join(",")')" = gurul && echo REPO_OK
  EXPECT: REPO_OK
  EVIDENCE: pending

- [ ] G9: The phone runs the Eve Link app, paired, and the old app (the previous bundle ID) is gone from it.
  EVIDENCE: pending
