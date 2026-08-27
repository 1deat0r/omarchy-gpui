# Omarchy to GPUI parity contract

Status: NOT MET

This is the acceptance contract for a complete replacement of the installed
Omarchy shell. The existing renderer is a foundation only. A 100% parity claim
is prohibited until every required behavior below is implemented and verified
against the reference shell in `/usr/share/omarchy/shell`.

Reference snapshot:

- Omarchy version observed locally: `4.0.1-1`
- Reference shell: `/usr/share/omarchy/shell`
- Reference config: `/usr/share/omarchy/config/omarchy/shell.json`
- User config: `~/.config/omarchy/shell.json`
- Runtime model: one long-lived Quickshell process hosted by Hyprland

## Completion rule

The port is complete only when:

1. every reference manifest is represented by a GPUI implementation;
2. every shell and plugin IPC target has equivalent return values and side
   effects;
3. every persistent state and reload rule matches the reference;
4. every system integration has an owned live adapter and a failure-path test;
5. visual, input, accessibility, multi-output, lifecycle, and session behavior
   are tested in a live Wayland session; and
6. the differential and integration gates in `GATES.md` are all green.

Passing compilation, a screenshot, or a smoke window is not parity evidence.

## Host and persistence

| Area | Reference behavior | GPUI acceptance requirement |
|---|---|---|
| Config selection | Valid version-1 user config fully replaces defaults; otherwise defaults or bundled fallback are used | Match selection, invalid-file handling, no-merge semantics, and live reload |
| Config persistence | Atomic writes to `~/.config/omarchy/shell.json`, version forced to 1 | Match serialization, preservation, mutation, and recovery behavior |
| Plugin discovery | Built-in and user plugin trees are scanned; the reserved `omarchy.*` first-party namespace cannot be shadowed by user plugins | Match manifest validation, source metadata, reserved IDs, clone routing, disabled state, and rescan behavior |
| Plugin loading | Bar widgets, panels, overlays, menus, services, and alternate bars load by manifest entry point | Provide equivalent typed GPUI registries and lifecycle ownership |
| Hot reload | File changes trigger debounced reload without duplicate instances | Match reload timing, pending calls, cleanup, and error fallback |
| Shell lifecycle | One long-lived process with shared services and plugin state | One process with equivalent shared state and clean shutdown/restart |
| App library | Desktop-entry discovery, search, launch feedback, and icon resolution | Match search, launch, feedback, and failure behavior |
| Theme | Shared colors, typography, spacing, border, popup, and corner policy | Match effective theme values and runtime theme changes |

## Reference plugin inventory

Every row is required. `pending` is intentional until its GPUI behavior and
tests are complete.

| Plugin ID | Kinds | Reference entry point | GPUI status |
|---|---|---|---|
| `omarchy.agents` | bar-widget | `agents/Panel.qml` | pending |
| `omarchy.background` | service | `background/Background.qml` | pending |
| `omarchy.bar` | bar | `bar/Bar.qml` | pending |
| `omarchy.active-window` | bar-widget | `bar/widgets/ActiveWindow.qml` | pending |
| `omarchy.indicators` | bar-widget | `bar/widgets/Indicators.qml` | pending |
| `omarchy.keyboard-layout` | bar-widget | `bar/widgets/KeyboardLayout.qml` | pending |
| `omarchy.microphone` | bar-widget | `bar/widgets/Microphone.qml` | pending |
| `omarchy.spacer` | bar-widget | `bar/widgets/Spacer.qml` | pending |
| `omarchy.system-update` | bar-widget | `bar/widgets/SystemUpdate.qml` | pending |
| `omarchy.tray` | bar-widget | `bar/widgets/Tray.qml` | pending |
| `omarchy.workspaces` | bar-widget | `bar/widgets/Workspaces.qml` | pending |
| `omarchy.clipboard` | overlay | `clipboard/Clipboard.qml` | pending |
| `omarchy.dev-gallery` | panel | `dev-gallery/GalleryPanel.qml` | pending |
| `omarchy.emojis` | overlay | `emojis/Emojis.qml` | pending |
| `omarchy.image-picker` | overlay | `image-picker/ImagePicker.qml` | pending |
| `omarchy.lock` | service | `lock/Service.qml` | pending |
| `omarchy.menu` | menu, bar-widget | `menu/Menu.qml`, `menu/BarWidget.qml` | pending |
| `omarchy.notifications` | service | `notifications/Service.qml` | pending |
| `omarchy.osd` | panel | `osd/Osd.qml` | pending |
| `omarchy.audio` | bar-widget | `panels/audio/Panel.qml` | pending |
| `omarchy.bluetooth` | bar-widget | `panels/bluetooth/Panel.qml` | pending |
| `omarchy.clock` | bar-widget | `panels/clock/BarWidget.qml` | pending |
| `omarchy.disk-speedtest` | panel | `panels/disk-speedtest/Panel.qml` | pending |
| `omarchy.dropbox` | bar-widget | `panels/dropbox/Panel.qml` | pending |
| `omarchy.monitor` | bar-widget | `panels/monitor/Panel.qml` | pending |
| `omarchy.network` | bar-widget | `panels/network/Panel.qml` | pending |
| `omarchy.power` | bar-widget | `panels/power/Panel.qml` | pending |
| `omarchy.speedtest` | panel | `panels/speedtest/Panel.qml` | pending |
| `omarchy.tailscale` | bar-widget | `panels/tailscale/Panel.qml` | pending |
| `omarchy.weather` | bar-widget | `panels/weather/BarWidget.qml` | pending |
| `omarchy.wifiqr` | panel | `panels/wifiqr/Panel.qml` | pending |
| `omarchy.polkit` | service | `polkit/PolkitAgent.qml` | pending |
| `omarchy.reminders` | overlay | `reminders/ReminderFlow.qml` | pending |
| `omarchy.battery` | service | `services/battery/Service.qml` | pending |
| `omarchy.idle` | service | `services/idle/Service.qml` | pending |
| `omarchy.media` | service, bar-widget | `services/media/Service.qml`, `services/media/BarWidget.qml` | pending |
| `omarchy.nightlight` | service | `services/nightlight/Service.qml` | pending |

## Shared UI and interaction surfaces

The GPUI implementation must cover the behavior, not merely the names, of:

- bar layout, drag/reorder, inline settings, custom command/QML modules,
  popouts, tray drawer, tooltips, active-window state, workspaces, indicators,
  microphone, keyboard layout, system update, clock, weather, agents, media,
  and all configured section behavior;
- panel cards, heroes, sections, separators, buttons, toggles, sliders,
  text fields, number fields, searchable dropdowns, multi-selects, confirmation
  dialogs, focus grabs, cursor surfaces, keyboard navigation, animations,
  responsive placement, and dismiss-on-outside-click behavior;
- menu search, JSONC menu loading, `when`/`checked` evaluation, action launch,
  extensions, and launch feedback;
- image picker, emoji picker, clipboard history, reminders, OSD, dev gallery,
  and all selection/cancel/done-file round trips;
- background image transitions, theme transitions, reveal effects, and output
  movement behavior; and
- lock screen, PAM password/fingerprint flows, polkit authentication, and
  fail-closed handling for privileged operations.

## System and compositor integrations

Each integration needs an owned adapter, event subscription, state mapping,
command path, and failure test:

- Hyprland IPC and events: workspaces, active window, monitor/output changes,
  raw events, focus, layer surfaces, and window lifecycle;
- Wayland layer-shell, popups, focus grabs, session lock, output scale, and
  multi-monitor placement;
- D-Bus/UPower: battery, charging state, low-battery warnings, and power
  profile switching;
- PipeWire/WirePlumber/MPRIS: output/input devices, volume/mute, media
  metadata, playback, and media OSD;
- NetworkManager/nmcli: Wi-Fi state, scanning, connect/disconnect, password
  prompts, QR generation, band selection, and speed tests;
- BlueZ: adapter/device discovery, pairing, trust, connect/disconnect, and
  Bluetooth state;
- system tray protocols: item discovery, icons, activation, menus, pinning,
  hiding, drawer expansion, and nested tray menus;
- notifications: urgency, actions, history, dismissal, persistence, and
  replacement/close semantics;
- idle/screensaver/lock/wake, night light, brightness, and monitor controls;
- desktop-entry/icon discovery, clipboard/files, wallpaper/theme tools,
  Dropbox/Tailscale, and external command error propagation; and
- native session startup, restart, logging, environment recovery, packaging,
  and reversible activation/rollback.

## IPC parity

The shell target must match `ping`, `summon`, `hide`, `toggle`, `call`,
`rescanPlugins`, `reloadConfig`, `toggleBarTransparency`, `setPluginEnabled`,
`enablePlugin`, `listPlugins`, and `togglePanelAt`, including argument typing,
unknown-target behavior, pending-load behavior, return strings, config writes,
clone routing, and side effects.

The following additional targets must be inventoried from the reference and
implemented with matching behavior: `image-selector`, `background`, `idle`,
`media`, `nightlight`, `osd`, `omarchy.audio`, `omarchy.bluetooth`,
`omarchy.clock`, `omarchy.monitor`, `omarchy.network`, `omarchy.power`,
`omarchy.weather`, `omarchy.tailscale`, `omarchy.dropbox`, and every target
registered by a loaded plugin.

## Current measured gap

The repository now has measured foundation slices for the bar surface,
configuration projection, clock, plugin discovery, shell IPC, Hyprland raw
events, MPRIS, StatusNotifier tray discovery/actions, and the optional
freedesktop notification service. These slices are intentionally documented in
their own source and runtime checks.

They do not yet satisfy complete plugin rows or the full system-integration
rows above: popup visuals, action dispatch, nested tray menus, expiry and
history lifecycle, multi-output behavior, failure-path coverage, and
replacement/rollback remain open. The parity gates must remain unmet until
those behaviors are implemented and verified.
