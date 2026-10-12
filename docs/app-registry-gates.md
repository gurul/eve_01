# Gates: apps are the owner's picks, and adding one is easy

OWNS: bridge/src/eve_bridge/app_registry.py (new), the apps' wiring sites (daemon.py, telegram.py, voice_agent.py, tool_gate.py, link_chat.py, miniapp.py, cli.py), bridge/tools/intent_eval.py, bridge/src/eve_bridge/app_template/** (new), bridge/tests/test_app_registry.py (new) and the tests the refactor touches, ios/EveLink/Shared/ControlsClient.swift, ios/EveLink/App/WorkView.swift, ios/EveLink/App/ControlSheets.swift, ios/EveLink/App/GenericAppSheet.swift (new), ios/EveLink/Tests/**, AGENTS.md, README.md, docs/adding-an-app.md (new), docs/apps.md, docs/app-chat.md, docs/app-registry-gates.md

Scope: the owner, 2026-10-12: "make it so that apps are variable in agents.md, people can build their own apps, these are just the apps i want", "so that it's really easy to build", and "include rea as a resource" (https://github.com/morluto/rea, reverse engineering for agents, MIT) and "this also as a resource" (https://github.com/gurul/hardware-logging, hwlog, serial logging for agents). Mapped the same day: an app (Lights, Music, Camera, Pika, TV) is a module plus hand wiring in ~12 files with no registry; the camera took 23 + 28 files. AGENTS.md's "Your own apps" means account connections, a different thing. So: one registry the wiring reads, a scaffold, a generic phone sheet so a new app needs no Swift, and docs that present the owner's apps as examples.

- [ ] A1: The registry and scaffold pass their tests: every existing app is declared once; chat, voice, the tool gate, the intent eval labels and /api/link/controls read the registry; `eve-bridge app new <name>` writes a module, tests and a registry line that pass as generated; an app switched off is gone everywhere.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_app_registry.py && echo REGISTRY_OK
  EXPECT: REGISTRY_OK

- [ ] A2: Nothing the models see changed: the full bridge suite (its byte-exact request-body tests included) and lint pass.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && (cd bridge && uv run ruff check src tests tools) && echo BRIDGE_OK
  EXPECT: BRIDGE_OK

- [ ] A3: The intent eval on the existing camera, location and science blind sets still clears each one's own bar after the refactor (same tools, same order, same words).
  CHECK: cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) sh -c '.venv/bin/python tools/intent_eval.py tests/fixtures/intent_camera_blind.json --require camera_aim=90 --require camera_photo=75 && .venv/bin/python tools/intent_eval.py tests/fixtures/intent_location_blind.json --require phone_location=90 && .venv/bin/python tools/intent_eval.py tests/fixtures/intent_science_blind.json --require science_search=90' && echo INTENT_SETS_OK
  EXPECT: INTENT_SETS_OK

- [ ] A4: The phone lists apps from the registry, opens the native sheet for apps that have one and the generic sheet for any other; the app's tests and the iPhone build pass.
  CHECK: sh ios/EveLink/Tests/run.sh && sh ios/EveLink/build.sh && echo APP_OK
  EXPECT: APP_OK

- [ ] A5: The docs are true: AGENTS.md presents the owner's apps as examples with their switches, links docs/adding-an-app.md, lists REA and hwlog as resources, and calls account connections "connected accounts"; every file path the new docs name exists.
  CHECK: python3 -c "import re,os,sys; t=open('AGENTS.md').read()+open('docs/adding-an-app.md').read(); need=['morluto/rea','gurul/hardware-logging','docs/adding-an-app.md','eve-bridge app new','onnected accounts']; miss=[n for n in need if n not in t]; paths=[p for p in set(re.findall(r'\x60((?:bridge|ios|docs)/[A-Za-z0-9_./-]+\.[a-z]+)\x60', t)) if not os.path.exists(p)]; print(miss or paths or 'DOCS_OK'); sys.exit(1 if miss or paths else 0)"
  EXPECT: DOCS_OK

- [ ] A6: No personal data or token-shaped strings in the diff.
  CHECK: git diff main | grep -v 'CHECK: git diff main' | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'sk-[A-Za-z0-9]{20,}', r'/Users/') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN

- [ ] A7: The owner's apps still work on his phone (Lights, Music, Camera) after the refactor.
  MANUAL: the owner's word.
