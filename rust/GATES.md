# Gates: rebuild the eve bridge in Rust

OWNS: rust/**, README.md, docs/rust.md

Scope: a Rust workspace under rust/ that ports every module of bridge/src/eve_bridge, builds, passes its ported tests, and is documented.

- [x] G1: plan.json maps every tracked Python module to exactly one crate
  CHECK: python3 tools/check_plan.py
  EXPECT: plan coverage passed
  CWD: rust
  EVIDENCE: 2026-10-20 e21e3bc: "206 python modules, 206 planned, 158 crates / plan coverage passed"

- [x] G2: the port checker rejects a placeholder crate and accepts a complete one
  CHECK: python3 tools/check_port.py --self-test
  EXPECT: self-test passed
  CWD: rust
  EVIDENCE: 2026-10-20 e21e3bc: "self-test passed"

- [x] G3: every crate exists, is complete per PARITY.md, names every Python definition, and has no placeholder
  CHECK: python3 tools/check_port.py
  EXPECT: port check passed
  CWD: rust
  EVIDENCE: 2026-10-20 e21e3bc: "158/158 crates pass / port check passed"

- [x] G4: the whole workspace builds, tests included
  CHECK: cargo build --workspace --all-targets 2>&1 && echo WORKSPACE_BUILD_OK
  EXPECT: WORKSPACE_BUILD_OK
  CWD: rust
  EVIDENCE: 2026-10-20 e21e3bc: WORKSPACE_BUILD_OK

- [x] G5: every ported test passes
  CHECK: cargo test --workspace 2>&1 && echo WORKSPACE_TESTS_OK
  EXPECT: WORKSPACE_TESTS_OK
  CWD: rust
  EVIDENCE: 2026-10-20 e21e3bc: 7538 passed, 0 failed, 61 ignored (live and differential); WORKSPACE_TESTS_OK

- [x] G6: the Rust eve-bridge binary offers every subcommand the Python CLI does
  CHECK: python3 tools/check_cli.py
  EXPECT: cli parity passed
  CWD: rust
  EVIDENCE: 2026-10-20 e21e3bc: full 286/286 and lite 18/18 command paths; "cli parity passed"

- [x] G7: README and docs/rust.md describe the Rust port and how to build and run it
  CHECK: python3 -c "import pathlib,sys; r=pathlib.Path('README.md').read_text(); d=pathlib.Path('docs/rust.md'); sys.exit(0 if 'docs/rust.md' in r and d.exists() and 'cargo build' in d.read_text() else 1)" && echo DOCS_OK
  EXPECT: DOCS_OK
  EVIDENCE: 2026-10-20 e21e3bc: DOCS_OK

- [x] G8: the Rust daemon runs in place of the Python one on the owner's Mac, against live state
  EVIDENCE: 2026-10-20: the launchd daemon runs the Rust build on the owner's Mac (startup in about 1.3 s, every component up); rollback kept
