# Omarchy GPUI

Gate status: **6/12 met** (6 remaining).

Parity status: **NOT MET**. This is a generated project status, not a completion claim.

This repository is an isolated Rust/GPUI implementation of the Omarchy shell.
The goal is behavioral parity with the installed Quickshell shell while keeping
Hyprland as the compositor during development and verification.

The root `README.md` is generated from this template before every commit, so it
is also the GitHub README. The generated status document is
[`docs/STATUS.md`](docs/STATUS.md).

## Repository boundary

The project does not modify the installed Omarchy tree, `~/.config/hypr`, the
active Quickshell shell, or the current session. Activation is blocked until
the parity gates and rollback test are complete.

## Run locally

```bash
cd /run/media/mustbearnold/Projects/Operating_Systems/Omarchy-GPUI
PATH=/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH \
WAYLAND_DISPLAY=wayland-1 cargo run --offline
```

Useful checks:

```bash
cargo run --offline -- --print-contract
cargo run --offline -- shell ping
WAYLAND_DISPLAY=wayland-1 cargo run --offline -- --smoke
cargo test --offline
```

## Project state

The native GPUI layer-shell bar, configuration loader, manifest discovery, and
initial command surface are implemented. The complete parity inventory is in
[`PARITY.md`](PARITY.md); the project must not be described as a complete
replacement until all parity gates are green.

## Development policy

The local `pre-commit` hook regenerates documentation, stages the generated
README/status files, and synchronizes the GitHub repository description through
the authenticated `gh` CLI. Commits are pushed to the public `origin` remote in
small verified increments.

See [`PLAN.md`](PLAN.md), [`GATES.md`](GATES.md), and
[`PARITY.md`](PARITY.md) for the active implementation and evidence ledgers.
