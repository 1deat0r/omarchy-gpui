# Gates: Omarchy GPUI shell parity port

OWNS: Cargo.toml, Cargo.lock, src/**, scripts/**, README.md, PLAN.md, PARITY.md, parity.json, GATES.md, .gitignore

Scope: deliver a complete, behaviorally verified Rust/GPUI replacement for the Omarchy shell while preserving the installed Omarchy and Hyprland activation boundary until parity is proven.

- [x] G1: project contains the documented Omarchy shell contract and GPUI implementation entrypoints
  CHECK: node scripts/verify-contract.mjs
  EXPECT: OMARCHY_GPUI_CONTRACT_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/run/media/mustbearnold/Projects/Operating_Systems/Omarchy-GPUI; path=88f97394e918/22 entries; output=OMARCHY_GPUI_CONTRACT_OK

- [x] G2: all Rust sources are formatted
  CHECK: PATH=/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH /home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo fmt --all -- --check && printf 'OMARCHY_GPUI_FMT_OK\n'
  EXPECT: OMARCHY_GPUI_FMT_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/run/media/mustbearnold/Projects/Operating_Systems/Omarchy-GPUI; path=88f97394e918/22 entries; output=OMARCHY_GPUI_FMT_OK

- [x] G3: the GPUI shell compiles with the pinned local dependency graph
  CHECK: PATH=/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH /home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo check --offline && printf 'OMARCHY_GPUI_CHECK_OK\n'
  EXPECT: OMARCHY_GPUI_CHECK_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/run/media/mustbearnold/Projects/Operating_Systems/Omarchy-GPUI; path=88f97394e918/22 entries; output=OMARCHY_GPUI_CHECK_OK | Finished `dev` profile [optimized + debuginfo] target(s) in 0.37s

- [x] G4: the contract parser reports the real Omarchy shell paths and IPC contract
  CHECK: PATH=/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH /home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo run --offline -- --print-contract
  EXPECT: OMARCHY_GPUI_CONTRACT_RUNTIME_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/run/media/mustbearnold/Projects/Operating_Systems/Omarchy-GPUI; path=88f97394e918/22 entries; output=Finished `dev` profile [optimized + debuginfo] target(s) in 0.27s | Running `target/debug/omarchy-gpui-shell --print-contract`

- [x] G5: the GPUI shell creates and renders a native Wayland window in the current Hyprland session
  CHECK: PATH=/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH env WAYLAND_DISPLAY=wayland-1 /home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo run --offline -- --smoke
  EXPECT: OMARCHY_GPUI_WAYLAND_SMOKE_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/run/media/mustbearnold/Projects/Operating_Systems/Omarchy-GPUI; path=88f97394e918/22 entries; output=Finished `dev` profile [optimized + debuginfo] target(s) in 0.27s | Running `target/debug/omarchy-gpui-shell --smoke`

- [ ] G7: the GPUI parity registry covers every reference manifest, shell IPC method, and required host feature
  CHECK: node scripts/audit-parity.mjs
  EXPECT: OMARCHY_GPUI_PARITY_AUDIT_OK
  EVIDENCE: pending

- [x] G8: the GPUI plugin registry discovers and validates every built-in and user plugin manifest with reference precedence
  CHECK: PATH=/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH /home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test --offline plugin_registry && printf 'OMARCHY_GPUI_PLUGIN_REGISTRY_OK\n'
  EXPECT: OMARCHY_GPUI_PLUGIN_REGISTRY_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/run/media/mustbearnold/Projects/Operating_Systems/Omarchy-GPUI; path=a29e3b47b6f6/22 entries; output=Finished `test` profile [optimized + debuginfo] target(s) in 0.25s | Running unittests src/main.rs (target/debug/deps/omarchy_gpui_shell-02e80364b5e13ace)

- [ ] G9: shell and plugin IPC behavior matches the reference target/method matrix, including state-changing side effects
  CHECK: node scripts/test-ipc-parity.mjs
  EXPECT: OMARCHY_GPUI_IPC_PARITY_OK
  EVIDENCE: pending

- [ ] G10: every bar widget, panel, overlay, menu, and service has a live GPUI implementation and feature-level tests
  CHECK: node scripts/test-plugin-parity.mjs
  EXPECT: OMARCHY_GPUI_PLUGIN_PARITY_OK
  EVIDENCE: pending

- [ ] G11: system integrations and failure paths match the reference in a live Wayland/Hyprland session
  CHECK: node scripts/test-system-parity.mjs
  EXPECT: OMARCHY_GPUI_SYSTEM_PARITY_OK
  EVIDENCE: pending

- [ ] G12: the GPUI shell can replace and restore the active Omarchy shell with a tested, reversible session procedure
  CHECK: node scripts/test-activation-rollback.mjs
  EXPECT: OMARCHY_GPUI_ACTIVATION_ROLLBACK_OK
  EVIDENCE: pending

- [x] G6: unit tests cover shell configuration selection and IPC command mapping
  CHECK: PATH=/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH /home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test --offline && printf 'OMARCHY_GPUI_TESTS_OK\n'
  EXPECT: OMARCHY_GPUI_TESTS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/run/media/mustbearnold/Projects/Operating_Systems/Omarchy-GPUI; path=88f97394e918/22 entries; output=Finished `test` profile [optimized + debuginfo] target(s) in 12.34s | Running unittests src/main.rs (target/debug/deps/omarchy_gpui_shell-02e80364b5e13ace)
