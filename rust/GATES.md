# Gates: rebuild the eve bridge in Rust

OWNS: rust/**, README.md, docs/rust.md

Scope: a Rust workspace under rust/ that ports every module of bridge/src/eve_bridge, builds, passes its ported tests, and is documented.

- [ ] G1: plan.json maps every tracked Python module to exactly one crate
  CHECK: python3 tools/check_plan.py
  EXPECT: plan coverage passed
  CWD: rust
  EVIDENCE: pending

- [ ] G2: the port checker rejects a placeholder crate and accepts a complete one
  CHECK: python3 tools/check_port.py --self-test
  EXPECT: self-test passed
  CWD: rust
  EVIDENCE: pending

- [ ] G3: every crate exists, is complete per PARITY.md, names every Python definition, and has no placeholder
  CHECK: python3 tools/check_port.py
  EXPECT: port check passed
  CWD: rust
  EVIDENCE: pending

- [ ] G4: the whole workspace builds, tests included
  CHECK: cargo build --workspace --all-targets 2>&1 && echo WORKSPACE_BUILD_OK
  EXPECT: WORKSPACE_BUILD_OK
  CWD: rust
  EVIDENCE: pending

- [ ] G5: every ported test passes
  CHECK: cargo test --workspace 2>&1 && echo WORKSPACE_TESTS_OK
  EXPECT: WORKSPACE_TESTS_OK
  CWD: rust
  EVIDENCE: pending

- [ ] G6: the Rust eve-bridge binary offers every subcommand the Python CLI does
  CHECK: python3 tools/check_cli.py
  EXPECT: cli parity passed
  CWD: rust
  EVIDENCE: pending

- [ ] G7: README and docs/rust.md describe the Rust port and how to build and run it
  CHECK: python3 -c "import pathlib,sys; r=pathlib.Path('README.md').read_text(); d=pathlib.Path('docs/rust.md'); sys.exit(0 if 'docs/rust.md' in r and d.exists() and 'cargo build' in d.read_text() else 1)" && echo DOCS_OK
  EXPECT: DOCS_OK
  EVIDENCE: pending

- [ ] G8: the Rust daemon runs in place of the Python one on the owner's Mac, against live state
  EVIDENCE: pending
