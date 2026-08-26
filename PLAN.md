# Omarchy GPUI shell parity port

## Boundary

This checkout is an isolated Rust/GPUI implementation of the Omarchy shell
host. It does not modify `/usr/share/omarchy`, `~/.config/hypr`, the installed
Quickshell shell, or Hyprland. Hyprland remains the compositor while parity is
developed and verified.

## Depth tree

### 1. Reference and parity contract

- Inventory every installed manifest, QML entry point, IPC target, system
  integration, persistence rule, and lifecycle behavior.
- Maintain `PARITY.md` and `parity.json` as the authoritative completion ledger.
- Require differential and live-session evidence before marking a row verified.

### 2. GPUI host and adapters

- Port the host lifecycle, plugin registry, config mutation, IPC, and shared
  services first.
- Port bar widgets, panels, overlays, menus, services, and system adapters in
  dependency order.
- Preserve Hyprland as the compositor until replacement behavior is proven.

### 3. Verification and activation boundary

- Run contract, format, compile, unit-test, differential, and live Wayland
  gates sequentially.
- Do not install or activate the GPUI shell until all required parity gates are
  met and a rollback path is tested.

## Not yet complete

- The complete required behavior is enumerated in `PARITY.md`; the current
  implementation is still a foundation and must not be described as parity.
