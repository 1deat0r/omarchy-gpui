use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{
    AppContext, Bounds, Context, Div, KeyDownEvent, Render, Stateful, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div,
    layer_shell::*, point, prelude::*, px, rgb, rgba, size,
};

use crate::config::{BarEntry, ShellSnapshot};
use crate::ipc::{IpcEvent, IpcEventReceiver};
use crate::menu::{MenuItem, MenuItemKind, MenuModel};
use crate::overlays::{
    OverlayAction, OverlayRow, clipboard_rows_from_path, default_clipboard_history_path,
    emoji_rows_from_path, image_rows_from_payload, parse_image_picker_payload, reminder_args,
    valid_reminder_minutes,
};
use crate::plugins::{PluginSnapshot, parse_network_qr};
use crate::system::{BluetoothDeviceAction, SystemAction, SystemSnapshot, run_action};

pub struct ShellView {
    snapshot: ShellSnapshot,
    system: SystemSnapshot,
    plugins: PluginSnapshot,
    clock: String,
    smoke: bool,
    reported_first_frame: bool,
    panel_id: Option<String>,
    panel_window: Option<WindowHandle<PanelView>>,
}

impl ShellView {
    pub fn new(
        snapshot: ShellSnapshot,
        smoke: bool,
        ipc_events: IpcEventReceiver,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = Arc::clone(&ipc_events);
        cx.spawn(async move |this, cx| {
            loop {
                let pending = events
                    .lock()
                    .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
                    .unwrap_or_default();
                if !pending.is_empty()
                    && this
                        .update(cx, |view, cx| {
                            for event in pending {
                                view.apply_ipc_event(event, cx);
                            }
                            cx.notify();
                        })
                        .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
        })
        .detach();

        let plugin_path = snapshot.omarchy_path.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let path = plugin_path.clone();
                let plugins = cx
                    .background_executor()
                    .spawn(async move { PluginSnapshot::collect(&path) })
                    .await;
                if this
                    .update(cx, |view, cx| {
                        view.plugins = plugins;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                let (snapshot, system) = cx
                    .background_executor()
                    .spawn(async { (ShellSnapshot::load(), SystemSnapshot::collect()) })
                    .await;
                if this
                    .update(cx, |view, cx| {
                        view.snapshot = snapshot;
                        view.system = system;
                        view.clock = local_clock();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
        .detach();

        Self {
            snapshot,
            system: SystemSnapshot::default(),
            plugins: PluginSnapshot::default(),
            clock: local_clock(),
            smoke,
            reported_first_frame: false,
            panel_id: None,
            panel_window: None,
        }
    }

    fn group(
        entries: &[BarEntry],
        clock: &str,
        system: &SystemSnapshot,
        plugins: &PluginSnapshot,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut group = div().flex().items_center().gap_1();
        if entries.is_empty() {
            return group.child(Self::chip("—", "empty"));
        }

        for entry in entries {
            if entry.id == "omarchy.workspaces" && !system.hyprland.workspaces.is_empty() {
                for workspace in &system.hyprland.workspaces {
                    let label = if workspace.name == system.hyprland.active_workspace {
                        format!("● {}", workspace.name)
                    } else {
                        format!("○ {}", workspace.name)
                    };
                    let workspace_name = workspace.name.clone();
                    let id = format!("omarchy-workspace-{}", workspace.name);
                    group = group.child(Self::chip(&label, &id).cursor_pointer().on_click(
                        move |_, _, _| {
                            let _ =
                                run_action(&SystemAction::FocusWorkspace(workspace_name.clone()));
                        },
                    ));
                }
                continue;
            }
            if !entry_visible(entry, plugins, system) {
                continue;
            }
            let label = label_for_entry(entry, clock, system, plugins);
            let _settings_are_preserved = &entry.settings;
            let id = entry.id.clone();
            if id == "omarchy.spacer" {
                let span = entry
                    .settings
                    .get("size")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(12.0)
                    .max(1.0);
                group = group.child(div().w(px(span as f32)).h(px(1.0)));
                continue;
            }
            group = group.child(Self::chip(&label, &entry.id).when(
                is_panel_capable(&id),
                |chip| {
                    chip.cursor_pointer()
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.toggle_panel(&id, cx);
                        }))
                },
            ));
        }
        group
    }

    fn toggle_panel(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.panel_id.as_deref() == Some(id) {
            self.close_panel(cx);
            return;
        }
        self.close_panel(cx);
        self.open_panel(id, "{}", cx);
    }

    fn open_panel(&mut self, id: &str, payload: &str, cx: &mut Context<Self>) {
        let panel_id = id.to_string();
        let panel_state = self.system.clone();
        let panel_plugins = self.plugins.clone();
        let panel_payload = payload.to_string();
        let panel_snapshot = self.snapshot.clone();
        let fullscreen_overlay = is_fullscreen_overlay(&panel_id);
        let namespace = if fullscreen_overlay {
            format!("omarchy-gpui-{}", panel_id.replace('.', "-"))
        } else {
            "omarchy-gpui-panel".to_string()
        };
        let Ok(handle) = cx.open_window(
            WindowOptions {
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: if fullscreen_overlay {
                        size(px(0.0), px(0.0))
                    } else {
                        size(px(520.0), px(560.0))
                    },
                })),
                app_id: Some("omarchy-gpui-panel".to_string()),
                window_background: if fullscreen_overlay {
                    WindowBackgroundAppearance::Transparent
                } else {
                    WindowBackgroundAppearance::Opaque
                },
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace,
                    layer: Layer::Overlay,
                    anchor: if fullscreen_overlay {
                        Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT
                    } else {
                        Anchor::TOP | Anchor::RIGHT
                    },
                    margin: if fullscreen_overlay {
                        Some((px(0.0), px(0.0), px(0.0), px(0.0)))
                    } else {
                        Some((px(54.0), px(12.0), px(12.0), px(12.0)))
                    },
                    keyboard_interactivity: if fullscreen_overlay {
                        KeyboardInteractivity::Exclusive
                    } else {
                        KeyboardInteractivity::OnDemand
                    },
                    ..Default::default()
                }),
                focus: true,
                is_movable: false,
                is_resizable: false,
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|cx| {
                    PanelView::new(
                        panel_id.clone(),
                        panel_state,
                        panel_plugins,
                        &panel_payload,
                        panel_snapshot,
                        cx,
                    )
                })
            },
        ) else {
            return;
        };
        self.panel_id = Some(id.to_string());
        self.panel_window = Some(handle);
        cx.notify();
    }

    fn close_panel(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.panel_window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.panel_id = None;
    }

    fn apply_ipc_event(&mut self, event: IpcEvent, cx: &mut Context<Self>) {
        match event {
            IpcEvent::Refresh => {}
            IpcEvent::Summon { id, payload } => {
                if is_panel_capable(&id) {
                    self.close_panel(cx);
                    self.open_panel(&id, &payload, cx);
                }
            }
            IpcEvent::Hide { id } => {
                if self.panel_id.as_deref() == Some(id.as_str()) {
                    self.close_panel(cx);
                }
            }
            IpcEvent::Toggle { id, payload } => {
                if is_panel_capable(&id) {
                    if self.panel_id.as_deref() == Some(id.as_str()) {
                        self.close_panel(cx);
                    } else {
                        self.close_panel(cx);
                        self.open_panel(&id, &payload, cx);
                    }
                }
            }
        }
    }

    fn chip(label: &str, id: &str) -> Stateful<Div> {
        div()
            .id(id.to_string())
            .flex()
            .items_center()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(0x27272a))
            .border_1()
            .border_color(rgb(0x3f3f46))
            .text_size(px(13.0))
            .text_color(rgb(0xf4f4f5))
            .child(label.to_string())
    }
}

impl Render for ShellView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.smoke && !self.reported_first_frame {
            println!("OMARCHY_GPUI_WAYLAND_SMOKE_OK");
            self.reported_first_frame = true;
        }

        let background = if self.snapshot.transparent {
            rgba(0x18181be8)
        } else {
            rgb(0x18181b)
        };

        div()
            .id("omarchy-gpui-shell")
            .size_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .bg(background)
            .text_color(rgb(0xf4f4f5))
            .child(Self::chip("GPUI", "omarchy-gpui-status"))
            .child(Self::group(
                &self.snapshot.left,
                &self.clock,
                &self.system,
                &self.plugins,
                cx,
            ))
            .child(div().flex_1().flex().justify_center().child(Self::group(
                &self.snapshot.center,
                &self.clock,
                &self.system,
                &self.plugins,
                cx,
            )))
            .child(div().flex().justify_end().child(Self::group(
                &self.snapshot.right,
                &self.clock,
                &self.system,
                &self.plugins,
                cx,
            )))
    }
}

struct PanelView {
    id: String,
    omarchy_path: PathBuf,
    system: SystemSnapshot,
    plugins: PluginSnapshot,
    message: String,
    menu: Option<MenuModel>,
    active_menu: String,
    menu_children: BTreeMap<String, Vec<MenuItem>>,
    filter_text: String,
    selected_menu_index: usize,
    overlay_rows: Vec<OverlayRow>,
    overlay_filterable: bool,
    qr_meta: String,
    qr_lines: Vec<String>,
    reminder_minutes: String,
    reminder_step_message: bool,
}

impl PanelView {
    fn new(
        id: String,
        system: SystemSnapshot,
        plugins: PluginSnapshot,
        payload: &str,
        snapshot: ShellSnapshot,
        cx: &mut Context<Self>,
    ) -> Self {
        let menu = (id == "omarchy.menu").then(MenuModel::load);
        let (overlay_rows, overlay_filterable) = overlay_rows_for(&id, payload, &snapshot);
        let (qr_meta, qr_lines) = if id == "omarchy.wifiqr" {
            wifi_qr_payload(&snapshot)
        } else {
            (String::new(), Vec::new())
        };
        let refresh_id = id.clone();
        let refresh_snapshot = snapshot.clone();
        let refresh_plugin_path = snapshot.omarchy_path.clone();
        let refresh_payload = payload.to_string();
        let refresh_qr_id = id.clone();
        let refresh_qr_snapshot = snapshot.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let system = cx
                    .background_executor()
                    .spawn(async { SystemSnapshot::collect() })
                    .await;
                if this
                    .update(cx, |view, cx| {
                        view.system = system;
                        if is_fullscreen_overlay(&refresh_id) && refresh_id != "omarchy.reminders" {
                            let (rows, filterable) =
                                overlay_rows_for(&refresh_id, &refresh_payload, &refresh_snapshot);
                            if !rows.is_empty() || refresh_id == "omarchy.clipboard" {
                                view.overlay_rows = rows;
                                view.overlay_filterable = filterable;
                                if view.selected_menu_index >= view.overlay_rows.len() {
                                    view.selected_menu_index = 0;
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                let path = refresh_plugin_path.clone();
                let plugins = cx
                    .background_executor()
                    .spawn(async move { PluginSnapshot::collect(&path) })
                    .await;
                let qr = if refresh_qr_id == "omarchy.wifiqr" {
                    Some(wifi_qr_payload(&refresh_qr_snapshot))
                } else {
                    None
                };
                if this
                    .update(cx, |view, cx| {
                        view.plugins = plugins;
                        if let Some((meta, rows)) = qr {
                            view.qr_meta = meta;
                            view.qr_lines = rows;
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
            }
        })
        .detach();
        let active_menu = menu
            .as_ref()
            .and_then(|_| serde_json::from_str::<serde_json::Value>(payload).ok())
            .and_then(|payload| {
                payload
                    .get("initialMenu")
                    .or_else(|| payload.get("menu"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .map(|route| {
                menu.as_ref()
                    .map(|model| model.resolve_route(&route))
                    .unwrap_or_else(|| "root".to_string())
            })
            .unwrap_or_else(|| "root".to_string());
        Self {
            id,
            omarchy_path: snapshot.omarchy_path,
            system,
            plugins,
            message: String::new(),
            menu,
            active_menu,
            menu_children: BTreeMap::new(),
            filter_text: String::new(),
            selected_menu_index: 0,
            overlay_rows,
            overlay_filterable,
            qr_meta,
            qr_lines,
            reminder_minutes: String::new(),
            reminder_step_message: false,
        }
    }

    fn execute(&mut self, action: SystemAction, cx: &mut Context<Self>) {
        self.message = match run_action(&action) {
            Ok(()) => "Action sent".to_string(),
            Err(error) => error,
        };
        cx.notify();
    }

    fn action_button(
        &self,
        label: &str,
        action: SystemAction,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = format!(
            "omarchy-gpui-action-{}",
            label.to_lowercase().replace(' ', "-")
        );
        div()
            .id(id)
            .cursor_pointer()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(0x27272a))
            .border_1()
            .border_color(rgb(0x3f3f46))
            .child(label.to_string())
            .on_click(cx.listener(move |view, _, _, cx| {
                view.execute(action.clone(), cx);
            }))
    }

    fn command_button(
        &self,
        label: &str,
        program: &str,
        args: &[&str],
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = format!(
            "omarchy-gpui-command-{}",
            label.to_lowercase().replace(' ', "-")
        );
        let program = self.omarchy_program(program);
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        div()
            .id(id)
            .cursor_pointer()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(0x27272a))
            .border_1()
            .border_color(rgb(0x3f3f46))
            .child(label.to_string())
            .on_click(cx.listener(move |view, _, _, cx| {
                view.message = match Command::new(&program).args(&args).spawn() {
                    Ok(_) => "Action started".to_string(),
                    Err(error) => format!("{}: {error}", program.display()),
                };
                cx.notify();
            }))
    }

    fn omarchy_program(&self, command: &str) -> PathBuf {
        let bundled = self.omarchy_path.join("bin").join(command);
        if bundled.is_file() {
            bundled
        } else {
            PathBuf::from(command)
        }
    }

    fn actions(&self, cx: &mut Context<Self>) -> Div {
        let mut actions = div().flex().gap_2().mt_4();
        match self.id.as_str() {
            "omarchy.audio" => {
                actions = actions
                    .child(self.action_button("Mute output", SystemAction::ToggleOutputMute, cx))
                    .child(self.action_button("Mute input", SystemAction::ToggleInputMute, cx))
                    .child(self.action_button("Set 50%", SystemAction::SetOutputVolume(50), cx));
            }
            "omarchy.bluetooth" => {
                actions = actions.child(self.action_button(
                    if self.system.bluetooth.powered {
                        "Power off"
                    } else {
                        "Power on"
                    },
                    SystemAction::SetBluetoothPower(!self.system.bluetooth.powered),
                    cx,
                ));
                for device in &self.system.bluetooth.devices {
                    let action = if device.connected {
                        BluetoothDeviceAction::Disconnect
                    } else {
                        BluetoothDeviceAction::Connect
                    };
                    let label = if device.connected {
                        format!("Disconnect {}", device.name)
                    } else {
                        format!("Connect {}", device.name)
                    };
                    actions = actions.child(self.action_button(
                        &label,
                        SystemAction::BluetoothDevice {
                            action,
                            address: device.address.clone(),
                        },
                        cx,
                    ));
                    if !device.connected {
                        actions = actions
                            .child(self.action_button(
                                &format!("Pair {}", device.name),
                                SystemAction::BluetoothDevice {
                                    action: BluetoothDeviceAction::Pair,
                                    address: device.address.clone(),
                                },
                                cx,
                            ))
                            .child(self.action_button(
                                &format!("Forget {}", device.name),
                                SystemAction::BluetoothDevice {
                                    action: BluetoothDeviceAction::Forget,
                                    address: device.address.clone(),
                                },
                                cx,
                            ));
                    }
                }
            }
            "omarchy.media" => {
                actions = actions
                    .child(self.action_button("Previous", SystemAction::MediaPrevious, cx))
                    .child(self.action_button("Play/Pause", SystemAction::MediaPlayPause, cx))
                    .child(self.action_button("Next", SystemAction::MediaNext, cx));
            }
            "omarchy.network" => {
                if !self.system.network.connection.is_empty()
                    && self.system.network.connection != "--"
                {
                    actions = actions.child(self.action_button(
                        "Reconnect",
                        SystemAction::ActivateNetwork(self.system.network.connection.clone()),
                        cx,
                    ));
                }
                for band in &self.system.network.band.available {
                    actions = actions.child(self.action_button(
                        &format!("Band {band}"),
                        SystemAction::SetNetworkBand(band.clone()),
                        cx,
                    ));
                }
                if let Some(device) = self
                    .system
                    .network
                    .wifi_networks
                    .iter()
                    .find(|network| !network.device.is_empty())
                    .map(|network| network.device.clone())
                {
                    actions = actions.child(self.action_button(
                        "Rescan Wi-Fi",
                        SystemAction::RescanWifi(device),
                        cx,
                    ));
                }
                for network in &self.system.network.wifi_networks {
                    if network.ssid.is_empty() || network.device.is_empty() {
                        continue;
                    }
                    let label = if network.connected {
                        format!("Disconnect {}", network.ssid)
                    } else {
                        format!("Connect {}", network.ssid)
                    };
                    let action = if network.connected {
                        SystemAction::DisconnectNetwork(network.device.clone())
                    } else {
                        SystemAction::ConnectNetwork {
                            ssid: network.ssid.clone(),
                            device: network.device.clone(),
                        }
                    };
                    actions = actions.child(self.action_button(&label, action, cx));
                    if network.known && !network.connected {
                        actions = actions.child(self.action_button(
                            &format!("Forget {}", network.ssid),
                            SystemAction::ForgetNetwork(network.ssid.clone()),
                            cx,
                        ));
                    }
                }
            }
            "omarchy.monitor" => {
                if self.system.display.brightness_available {
                    actions = actions.child(self.action_button(
                        "Brightness 50%",
                        SystemAction::SetBrightness {
                            monitor: self.system.display.focused_monitor.clone(),
                            percent: 50,
                        },
                        cx,
                    ));
                }
                if !self.system.display.focused_monitor.is_empty() {
                    actions = actions
                        .child(self.action_button(
                            "Scale 1.25",
                            SystemAction::SetMonitorScale("1.25".to_string()),
                            cx,
                        ))
                        .child(self.action_button("Text 12px", SystemAction::SetTextSize(12), cx));
                    actions = actions.child(self.action_button(
                        "Toggle nightlight",
                        SystemAction::ToggleNightlight,
                        cx,
                    ));
                }
                for display in &self.system.display.displays {
                    let action_label = if display.enabled {
                        format!("Disable {}", display.name)
                    } else {
                        format!("Enable {}", display.name)
                    };
                    actions = actions.child(self.action_button(
                        &action_label,
                        SystemAction::ToggleDisplay {
                            name: display.name.clone(),
                            enabled: display.enabled,
                        },
                        cx,
                    ));
                }
            }
            "omarchy.power" => {
                for profile in &self.system.power.profiles {
                    actions = actions.child(self.action_button(
                        &format!("Use {}", profile.name),
                        SystemAction::SetPowerProfile {
                            profile: profile.name.clone(),
                            on_battery: !self.system.battery.charging,
                        },
                        cx,
                    ));
                }
            }
            "omarchy.agents" => {
                actions = actions
                    .child(self.command_button("Launch agent", "omarchy-agent", &["--pick"], cx))
                    .child(self.command_button(
                        "Refresh usage",
                        "omarchy-agent-usage-update",
                        &["--force"],
                        cx,
                    ));
            }
            "omarchy.dropbox" => {
                if self.plugins.dropbox.installed {
                    actions = actions.child(if self.plugins.dropbox.running {
                        self.command_button("Pause syncing", "dropbox-cli", &["stop"], cx)
                    } else {
                        self.command_button("Resume syncing", "dropbox-cli", &["start"], cx)
                    });
                    actions = actions.child(self.command_button(
                        "Start or login",
                        "dropbox-cli",
                        &["start"],
                        cx,
                    ));
                }
            }
            "omarchy.tailscale" => {
                if self.plugins.tailscale.installed {
                    actions = actions.child(if self.plugins.tailscale.running {
                        self.command_button("Disconnect", "tailscale", &["down"], cx)
                    } else {
                        self.command_button("Connect", "tailscale", &["up"], cx)
                    });
                    actions = actions.child(self.command_button(
                        "Refresh status",
                        "tailscale",
                        &["status", "--json"],
                        cx,
                    ));
                }
            }
            "omarchy.system-update" => {
                actions = actions.child(self.command_button(
                    "Open updater",
                    "omarchy-launch-floating-terminal-with-presentation",
                    &["omarchy-update"],
                    cx,
                ));
            }
            "omarchy.weather" => {
                actions = actions
                    .child(self.command_button(
                        "Refresh weather",
                        "omarchy-weather-status",
                        &[],
                        cx,
                    ))
                    .child(self.command_button(
                        "Auto location",
                        "omarchy-weather-location",
                        &["--clear"],
                        cx,
                    ));
            }
            _ => {}
        }
        actions
    }

    fn is_overlay(&self) -> bool {
        is_fullscreen_overlay(&self.id)
    }

    fn visible_overlay_rows(&mut self) -> Vec<OverlayRow> {
        let query = self.filter_text.trim().to_lowercase();
        let rows = self
            .overlay_rows
            .iter()
            .filter(|row| {
                query.is_empty()
                    || format!("{} {} {}", row.id, row.label, row.detail)
                        .to_lowercase()
                        .contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        if rows.is_empty() {
            self.selected_menu_index = 0;
        } else if self.selected_menu_index >= rows.len() {
            self.selected_menu_index = rows.len() - 1;
        }
        rows
    }

    fn overlay_content(&mut self, cx: &mut Context<Self>) -> Div {
        let rows = self.visible_overlay_rows();
        let mut content = div().flex().flex_col().gap_2().mt_3();

        if self.overlay_filterable || self.id == "omarchy.reminders" {
            let prompt = if self.id == "omarchy.reminders" {
                if self.reminder_step_message {
                    "Reminder message…"
                } else {
                    "Remind in minutes…"
                }
            } else {
                "Search…"
            };
            content = content.child(
                div()
                    .id("omarchy-gpui-overlay-search")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x27272a))
                    .text_color(if self.filter_text.is_empty() {
                        rgb(0x71717a)
                    } else {
                        rgb(0xf4f4f5)
                    })
                    .child(if self.filter_text.is_empty() {
                        prompt.to_string()
                    } else {
                        self.filter_text.clone()
                    }),
            );
        }

        if self.id == "omarchy.emojis" {
            let mut grid = div().flex().flex_wrap().gap_2();
            for (index, row) in rows.into_iter().enumerate() {
                let action = row.action.clone();
                let row_id = row.id.clone();
                grid = grid.child(
                    div()
                        .id(format!("omarchy-gpui-overlay-{row_id}"))
                        .cursor_pointer()
                        .w(px(56.0))
                        .h(px(48.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .bg(if index == self.selected_menu_index {
                            rgb(0x3f3f46)
                        } else {
                            rgb(0x27272a)
                        })
                        .text_size(px(24.0))
                        .child(row.label)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.activate_overlay_action(action.clone(), window, cx);
                        })),
                );
            }
            if self.overlay_rows.is_empty() {
                grid = grid.child(
                    div()
                        .text_color(rgb(0xa1a1aa))
                        .child("No emoji data available."),
                );
            }
            return content.child(grid);
        }

        if self.id == "omarchy.reminders" {
            return content.child(div().text_color(rgb(0xa1a1aa)).child(
                if self.reminder_step_message {
                    "Press Enter to schedule, or Esc to go back."
                } else {
                    "Enter a positive number of minutes, then optionally add a message."
                },
            ));
        }

        let rows_empty = rows.is_empty();
        let mut rows_view = div().flex().flex_col().gap_2();
        for (index, row) in rows.into_iter().enumerate() {
            let action = row.action.clone();
            let row_id = row.id.clone();
            let detail = row.detail.clone();
            rows_view = rows_view.child(
                div()
                    .id(format!("omarchy-gpui-overlay-{row_id}"))
                    .cursor_pointer()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(if index == self.selected_menu_index {
                        rgb(0x3f3f46)
                    } else {
                        rgb(0x27272a)
                    })
                    .border_1()
                    .border_color(rgb(0x3f3f46))
                    .child(row.label)
                    .when(!detail.is_empty(), |row_view| {
                        row_view.child(
                            div()
                                .mt_1()
                                .text_size(px(11.0))
                                .text_color(rgb(0xa1a1aa))
                                .child(detail.clone()),
                        )
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.activate_overlay_action(action.clone(), window, cx);
                    })),
            );
        }
        if rows_empty {
            rows_view = rows_view.child(
                div()
                    .text_color(rgb(0xa1a1aa))
                    .child("No entries available."),
            );
        }
        content.child(rows_view)
    }

    fn wifi_qr_content(&self) -> Div {
        let mut content = div().flex().flex_col().gap_2().mt_3();
        if !self.qr_meta.is_empty() {
            content = content.child(div().text_color(rgb(0xa1a1aa)).child(self.qr_meta.clone()));
        }
        if self.qr_lines.is_empty() {
            return content.child(
                div()
                    .text_color(rgb(0xfca5a5))
                    .child("No QR data available."),
            );
        }
        let mut qr = String::new();
        for line in &self.qr_lines {
            for cell in line.chars() {
                qr.push_str(if cell == '1' { "██" } else { "  " });
            }
            qr.push('\n');
        }
        content.child(div().flex().justify_center().text_size(px(7.0)).child(qr))
    }

    fn activate_overlay_action(
        &mut self,
        action: OverlayAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match action {
            OverlayAction::ClipboardPasteText { index } => self.spawn_omarchy(
                "omarchy-clipboard-paste-text",
                &[
                    "--shift-insert".to_string(),
                    "--history-index".to_string(),
                    index.to_string(),
                ],
            ),
            OverlayAction::ClipboardPasteImage { mime, path } => {
                self.spawn_omarchy("omarchy-clipboard-paste-file", &[mime, path])
            }
            OverlayAction::EmojiInsert(emoji) => {
                self.spawn_omarchy("omarchy-menu-emoji-insert", &[emoji])
            }
            OverlayAction::SelectImage {
                path,
                selection_file,
                done_file,
            } => write_image_selection(&path, &selection_file, &done_file),
        };
        match result {
            Ok(()) => window.remove_window(),
            Err(error) => self.message = error,
        }
        cx.notify();
    }

    fn spawn_omarchy(&self, command: &str, args: &[String]) -> Result<(), String> {
        Command::new(self.omarchy_path.join("bin").join(command))
            .args(args)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("{command}: {error}"))
    }

    fn handle_overlay_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => {
                if !self.filter_text.is_empty() {
                    self.filter_text.clear();
                    self.selected_menu_index = 0;
                    cx.notify();
                } else if self.reminder_step_message {
                    self.reminder_step_message = false;
                    self.reminder_minutes.clear();
                    cx.notify();
                } else {
                    window.remove_window();
                }
                return;
            }
            "backspace" => {
                if self.filter_text.pop().is_some() {
                    self.selected_menu_index = 0;
                    cx.notify();
                }
                return;
            }
            "up" | "k" => {
                self.move_overlay_selection(-1);
                cx.notify();
                return;
            }
            "down" | "j" => {
                self.move_overlay_selection(1);
                cx.notify();
                return;
            }
            "enter" | "return" => {
                if self.id == "omarchy.reminders" {
                    self.submit_reminder(window, cx);
                } else if let Some(row) = self.visible_overlay_rows().get(self.selected_menu_index)
                {
                    self.activate_overlay_action(row.action.clone(), window, cx);
                }
                return;
            }
            _ => {}
        }
        if event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.platform
        {
            return;
        }
        if let Some(character) = event
            .keystroke
            .key_char
            .as_deref()
            .or_else(|| (!key.is_empty() && key.chars().count() == 1).then_some(key))
            && !character.chars().any(char::is_control)
        {
            self.filter_text.push_str(character);
            self.selected_menu_index = 0;
            cx.notify();
        }
    }

    fn move_overlay_selection(&mut self, delta: i32) {
        let count = self.visible_overlay_rows().len();
        if count == 0 {
            self.selected_menu_index = 0;
            return;
        }
        let next = self.selected_menu_index as i32 + delta;
        self.selected_menu_index = next.clamp(0, count as i32 - 1) as usize;
    }

    fn submit_reminder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reminder_step_message {
            if self.filter_text.trim().is_empty() {
                window.remove_window();
            } else if let Some(minutes) = valid_reminder_minutes(&self.filter_text) {
                self.reminder_minutes = minutes;
                self.reminder_step_message = true;
                self.filter_text.clear();
                cx.notify();
            } else {
                self.message = "Enter a positive number of minutes".to_string();
                cx.notify();
            }
            return;
        }

        let Some(args) = reminder_args(&self.reminder_minutes, &self.filter_text) else {
            self.message = "Reminder minutes are invalid".to_string();
            cx.notify();
            return;
        };
        match self.spawn_omarchy("omarchy-reminder", &args) {
            Ok(()) => window.remove_window(),
            Err(error) => self.message = error,
        }
        cx.notify();
    }

    fn menu_content(&mut self, cx: &mut Context<Self>) -> Div {
        let Some(model) = self.menu.clone() else {
            return div();
        };
        let active_menu = self.active_menu.clone();
        let items = self.visible_menu_items(&model);
        let mut content = div().flex().flex_col().gap_1().mt_3();

        content = content.child(
            div()
                .id("omarchy-gpui-menu-search")
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(0x27272a))
                .text_color(rgb(0xa1a1aa))
                .child(if self.filter_text.is_empty() {
                    "Type to search…".to_string()
                } else {
                    format!("Search: {}", self.filter_text)
                }),
        );

        if active_menu != "root" {
            let parent = model
                .parent(&active_menu)
                .unwrap_or_else(|| "root".to_string());
            content = content.child(
                div()
                    .id("omarchy-gpui-menu-back")
                    .cursor_pointer()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x27272a))
                    .child("‹ Back")
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.active_menu = parent.clone();
                        cx.notify();
                    })),
            );
        }

        for (index, item) in items.into_iter().enumerate() {
            let item_id = item.id.clone();
            let label = menu_label(&item);
            let description = item.description.clone();
            let row = div()
                .id(format!("omarchy-gpui-menu-item-{item_id}"))
                .cursor_pointer()
                .flex()
                .flex_col()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(if index == self.selected_menu_index {
                    rgb(0x3f3f46)
                } else {
                    rgb(0x27272a)
                })
                .border_1()
                .border_color(rgb(0x3f3f46))
                .child(label)
                .when(!description.is_empty(), |row| {
                    row.child(
                        div()
                            .mt_1()
                            .text_size(px(11.0))
                            .text_color(rgb(0xa1a1aa))
                            .child(description.clone()),
                    )
                })
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.activate_menu_item(&item_id, window, cx);
                }));
            content = content.child(row);
        }
        content
    }

    fn visible_menu_items(&mut self, model: &MenuModel) -> Vec<MenuItem> {
        let active_menu = self.active_menu.clone();
        let items = self
            .menu_children
            .entry(active_menu.clone())
            .or_insert_with(|| model.children_with_providers(&active_menu))
            .clone()
            .into_iter()
            .filter(|item| MenuModel::evaluate_guard(&item.when))
            .filter(|item| menu_matches_filter(item, &self.filter_text))
            .collect::<Vec<_>>();
        if items.is_empty() {
            self.selected_menu_index = 0;
        } else if self.selected_menu_index >= items.len() {
            self.selected_menu_index = items.len() - 1;
        }
        items
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.menu.clone() else {
            if self.is_overlay() {
                self.handle_overlay_key(event, window, cx);
            } else if event.keystroke.key == "escape" {
                window.remove_window();
            }
            return;
        };
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => {
                window.remove_window();
                return;
            }
            "backspace" => {
                if self.filter_text.is_empty() {
                    if let Some(parent) = model.parent(&self.active_menu) {
                        self.active_menu = parent;
                        self.selected_menu_index = 0;
                    }
                } else {
                    self.filter_text.pop();
                    self.selected_menu_index = 0;
                }
                cx.notify();
                return;
            }
            "up" | "k" => {
                self.move_menu_selection(-1, &model);
                cx.notify();
                return;
            }
            "down" | "j" => {
                self.move_menu_selection(1, &model);
                cx.notify();
                return;
            }
            "enter" | "return" => {
                let items = self.visible_menu_items(&model);
                if let Some(item) = items.get(self.selected_menu_index) {
                    self.activate_menu_item(&item.id, window, cx);
                }
                return;
            }
            "left" | "h" if self.filter_text.is_empty() => {
                if let Some(parent) = model.parent(&self.active_menu) {
                    self.active_menu = parent;
                    self.selected_menu_index = 0;
                    cx.notify();
                }
                return;
            }
            _ => {}
        }
        if event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.platform
        {
            return;
        }
        if let Some(character) = event
            .keystroke
            .key_char
            .as_deref()
            .or_else(|| (!key.is_empty() && key.chars().count() == 1).then_some(key))
            && !character.chars().any(char::is_control)
        {
            self.filter_text.push_str(character);
            self.selected_menu_index = 0;
            cx.notify();
        }
    }

    fn move_menu_selection(&mut self, delta: i32, model: &MenuModel) {
        let count = self.visible_menu_items(model).len();
        if count == 0 {
            self.selected_menu_index = 0;
            return;
        }
        let next = self.selected_menu_index as i32 + delta;
        self.selected_menu_index = next.clamp(0, count as i32 - 1) as usize;
    }

    fn activate_menu_item(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let item = self
            .menu
            .as_ref()
            .and_then(|menu| menu.item(id).cloned())
            .or_else(|| {
                self.menu_children
                    .get(&self.active_menu)
                    .and_then(|items| items.iter().find(|item| item.id == id).cloned())
            });
        let Some(item) = item else {
            self.message = format!("Unknown menu item: {id}");
            cx.notify();
            return;
        };
        match item.kind {
            MenuItemKind::Menu => {
                self.active_menu = item.id;
                self.filter_text.clear();
                self.selected_menu_index = 0;
            }
            MenuItemKind::Link => {
                self.active_menu = item.target;
                self.filter_text.clear();
                self.selected_menu_index = 0;
            }
            MenuItemKind::Action => match MenuModel::run_action(&item.action) {
                Ok(()) => {
                    window.remove_window();
                }
                Err(error) => {
                    self.message = error;
                }
            },
        }
        cx.notify();
    }
}

impl Render for PanelView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .menu
            .as_ref()
            .and_then(|menu| menu.item(&self.active_menu))
            .map(|item| {
                if item.title.is_empty() {
                    item.label.clone()
                } else {
                    item.title.clone()
                }
            })
            .unwrap_or_else(|| label_for(&self.id).to_string());
        let content = if self.menu.is_some() {
            self.menu_content(cx)
        } else if self.id == "omarchy.wifiqr" {
            self.wifi_qr_content()
        } else if self.is_overlay() {
            self.overlay_content(cx)
        } else {
            let mut rows = div().flex().flex_col().gap_2().mt_3();
            for (label, value) in panel_rows(&self.id, &self.system, &self.plugins) {
                rows = rows.child(
                    div()
                        .flex()
                        .justify_between()
                        .gap_4()
                        .child(div().text_color(rgb(0xa1a1aa)).child(label))
                        .child(div().text_color(rgb(0xf4f4f5)).child(value)),
                );
            }
            rows
        };
        let content = content.flex_1();
        let actions = if self.is_overlay() {
            div()
        } else {
            self.actions(cx)
        };
        let fullscreen = self.is_overlay();
        let header = div()
            .flex()
            .justify_between()
            .items_center()
            .child(div().text_size(px(18.0)).child(title))
            .child(
                div()
                    .id("omarchy-gpui-panel-close")
                    .cursor_pointer()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x27272a))
                    .child("CLOSE")
                    .on_click(|_, window, _| window.remove_window()),
            );
        let body = div()
            .id("omarchy-gpui-panel-body")
            .size_full()
            .flex()
            .flex_col()
            .p_5()
            .bg(rgb(0x18181b))
            .text_color(rgb(0xf4f4f5))
            .on_key_down(cx.listener(Self::handle_key))
            .child(header)
            .child(content)
            .child(actions)
            .when(!self.message.is_empty(), |panel| {
                panel.child(
                    div()
                        .mt_3()
                        .text_color(rgb(0xa1a1aa))
                        .child(self.message.clone()),
                )
            });

        if fullscreen {
            div()
                .id("omarchy-gpui-overlay-scrim")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x000000b8))
                .child(
                    div()
                        .id("omarchy-gpui-overlay-card")
                        .w(px(820.0))
                        .h(px(620.0))
                        .rounded_lg()
                        .overflow_hidden()
                        .child(body),
                )
        } else {
            body
        }
    }
}

fn is_panel_capable(id: &str) -> bool {
    matches!(
        id,
        "omarchy.agents"
            | "omarchy.audio"
            | "omarchy.bluetooth"
            | "omarchy.clipboard"
            | "omarchy.clock"
            | "omarchy.dev-gallery"
            | "omarchy.dropbox"
            | "omarchy.emojis"
            | "omarchy.image-picker"
            | "omarchy.menu"
            | "omarchy.monitor"
            | "omarchy.network"
            | "omarchy.osd"
            | "omarchy.power"
            | "omarchy.reminders"
            | "omarchy.disk-speedtest"
            | "omarchy.speedtest"
            | "omarchy.tailscale"
            | "omarchy.weather"
            | "omarchy.wifiqr"
            | "omarchy.media"
    )
}

fn is_fullscreen_overlay(id: &str) -> bool {
    matches!(
        id,
        "omarchy.clipboard"
            | "omarchy.dev-gallery"
            | "omarchy.disk-speedtest"
            | "omarchy.emojis"
            | "omarchy.image-picker"
            | "omarchy.osd"
            | "omarchy.reminders"
            | "omarchy.speedtest"
            | "omarchy.wifiqr"
    )
}

fn overlay_rows_for(id: &str, payload: &str, snapshot: &ShellSnapshot) -> (Vec<OverlayRow>, bool) {
    match id {
        "omarchy.clipboard" => (
            clipboard_rows_from_path(&default_clipboard_history_path(), ""),
            true,
        ),
        "omarchy.emojis" => (
            emoji_rows_from_path(
                &snapshot
                    .omarchy_path
                    .join("shell/plugins/emojis/emojis.json"),
                "",
            ),
            true,
        ),
        "omarchy.image-picker" => {
            let mut image_payload = parse_image_picker_payload(payload);
            if image_payload.rows.trim().is_empty() && !image_payload.image_dirs.trim().is_empty() {
                let list_script = snapshot
                    .omarchy_path
                    .join("shell/plugins/image-picker/list.sh");
                if let Ok(output) = Command::new(list_script)
                    .arg(&image_payload.image_dirs)
                    .output()
                    && output.status.success()
                {
                    image_payload.rows = String::from_utf8_lossy(&output.stdout).into_owned();
                }
            }
            let filterable = image_payload.filterable;
            (image_rows_from_payload(&image_payload), filterable)
        }
        _ => (Vec::new(), false),
    }
}

fn wifi_qr_payload(snapshot: &ShellSnapshot) -> (String, Vec<String>) {
    let bundled = snapshot.omarchy_path.join("bin/omarchy-network-qr");
    let program = if bundled.is_file() {
        bundled
    } else {
        PathBuf::from("omarchy-network-qr")
    };
    match Command::new(program).arg("--meta").output() {
        Ok(output) if output.status.success() => {
            parse_network_qr(&String::from_utf8_lossy(&output.stdout))
        }
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            (
                if detail.is_empty() {
                    "Wi-Fi QR unavailable".to_string()
                } else {
                    format!("Wi-Fi QR unavailable: {detail}")
                },
                Vec::new(),
            )
        }
        Err(error) => (format!("Wi-Fi QR unavailable: {error}"), Vec::new()),
    }
}

fn write_image_selection(path: &str, selection_file: &str, done_file: &str) -> Result<(), String> {
    if selection_file.trim().is_empty() {
        return truncate_file(done_file);
    }
    write_text_file(selection_file, &format!("{path}\n"))?;
    truncate_file(done_file)
}

fn write_text_file(path: &str, contents: &str) -> Result<(), String> {
    let target = PathBuf::from(path);
    if let Some(parent) = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    fs::write(&target, contents).map_err(|error| format!("write {}: {error}", target.display()))
}

fn truncate_file(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Ok(());
    }
    write_text_file(path, "")
}

fn menu_label(item: &MenuItem) -> String {
    let mut label = if item.icon.is_empty() {
        item.label.clone()
    } else {
        format!("{}  {}", item.icon, item.label)
    };
    if !item.checked.is_empty() && MenuModel::evaluate_guard(&item.checked) {
        label.push_str("  ✓");
    }
    if label.is_empty() {
        item.id.clone()
    } else {
        label
    }
}

fn menu_matches_filter(item: &MenuItem, filter: &str) -> bool {
    let query = filter.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {} {}",
        item.id,
        item.label,
        item.title,
        item.description,
        item.aliases.join(" ")
    )
    .to_lowercase();
    query.split_whitespace().all(|term| haystack.contains(term))
}

fn panel_rows(
    id: &str,
    system: &SystemSnapshot,
    plugins: &PluginSnapshot,
) -> Vec<(String, String)> {
    match id {
        "omarchy.audio" => vec![
            (
                "Output".to_string(),
                system
                    .audio
                    .output_percent
                    .map(|value| format!("{value}%"))
                    .unwrap_or_else(|| "Unavailable".to_string()),
            ),
            ("Output mute".to_string(), yes_no(system.audio.output_muted)),
            (
                "Input".to_string(),
                system
                    .audio
                    .input_percent
                    .map(|value| format!("{value}%"))
                    .unwrap_or_else(|| "Unavailable".to_string()),
            ),
        ],
        "omarchy.bluetooth" => vec![
            ("Powered".to_string(), yes_no(system.bluetooth.powered)),
            (
                "Connected".to_string(),
                system.bluetooth.connected_devices.to_string(),
            ),
        ],
        "omarchy.network" => vec![
            (
                "Device".to_string(),
                display_or_dash(&system.network.device),
            ),
            ("Type".to_string(), display_or_dash(&system.network.kind)),
            (
                "Connection".to_string(),
                display_or_dash(&system.network.connection),
            ),
            (
                "Signal".to_string(),
                system
                    .network
                    .signal_percent
                    .map(|value| format!("{value}%"))
                    .unwrap_or_else(|| "—".to_string()),
            ),
            (
                "Band".to_string(),
                display_or_dash(&system.network.band.current),
            ),
        ],
        "omarchy.monitor" => vec![
            (
                "Focused".to_string(),
                display_or_dash(&system.display.focused_monitor),
            ),
            (
                "Brightness".to_string(),
                system
                    .display
                    .brightness_percent
                    .map(|value| format!("{value}%"))
                    .unwrap_or_else(|| "Unavailable".to_string()),
            ),
            (
                "Scale".to_string(),
                display_or_dash(&system.display.monitor_scale),
            ),
            (
                "Displays".to_string(),
                system.display.displays.len().to_string(),
            ),
        ],
        "omarchy.power" => vec![
            (
                "Battery".to_string(),
                system
                    .battery
                    .percentage
                    .map(|value| format!("{value}%"))
                    .unwrap_or_else(|| "Unavailable".to_string()),
            ),
            ("State".to_string(), display_or_dash(&system.battery.state)),
            (
                "Profile".to_string(),
                display_or_dash(&system.power.active_profile),
            ),
        ],
        "omarchy.media" => vec![
            ("Player".to_string(), display_or_dash(&system.media.player)),
            ("Status".to_string(), display_or_dash(&system.media.status)),
            ("Artist".to_string(), display_or_dash(&system.media.artist)),
            ("Title".to_string(), display_or_dash(&system.media.title)),
        ],
        "omarchy.clock" => vec![(
            "Monitor".to_string(),
            display_or_dash(&system.hyprland.monitor),
        )],
        "omarchy.agents" => vec![
            (
                "Default agent".to_string(),
                display_or_dash(&plugins.agents.default_agent),
            ),
            (
                "Usage adapter".to_string(),
                if plugins.agents.available {
                    "available".to_string()
                } else {
                    "unavailable".to_string()
                },
            ),
        ],
        "omarchy.keyboard-layout" => vec![
            (
                "Layout".to_string(),
                display_or_dash(&plugins.keyboard.layout_full),
            ),
            (
                "Keyboard".to_string(),
                display_or_dash(&plugins.keyboard.keyboard_name),
            ),
            (
                "Layouts".to_string(),
                yes_no(plugins.keyboard.multiple_layouts),
            ),
        ],
        "omarchy.weather" => vec![
            (
                "Location".to_string(),
                display_or_dash(&plugins.weather.location),
            ),
            (
                "Report".to_string(),
                display_or_dash(&plugins.weather.status),
            ),
        ],
        "omarchy.system-update" => vec![
            ("Available".to_string(), yes_no(plugins.update.available)),
            (
                "Detail".to_string(),
                display_or_dash(&plugins.update.detail),
            ),
        ],
        "omarchy.dropbox" => vec![
            ("Installed".to_string(), yes_no(plugins.dropbox.installed)),
            ("Running".to_string(), yes_no(plugins.dropbox.running)),
        ],
        "omarchy.tailscale" => vec![
            ("Installed".to_string(), yes_no(plugins.tailscale.installed)),
            (
                "State".to_string(),
                display_or_dash(&plugins.tailscale.status),
            ),
            (
                "Self".to_string(),
                display_or_dash(&plugins.tailscale.self_name),
            ),
            ("Peers".to_string(), plugins.tailscale.peers.to_string()),
        ],
        _ => vec![("State".to_string(), "GPUI adapter active".to_string())],
    }
}

fn display_or_dash(value: &str) -> String {
    if value.is_empty() || value == "--" {
        "—".to_string()
    } else {
        value.to_string()
    }
}

fn yes_no(value: bool) -> String {
    if value {
        "Yes".to_string()
    } else {
        "No".to_string()
    }
}

fn entry_visible(entry: &BarEntry, plugins: &PluginSnapshot, system: &SystemSnapshot) -> bool {
    match entry.id.as_str() {
        "omarchy.keyboard-layout" => {
            plugins.keyboard.available && plugins.keyboard.multiple_layouts
        }
        "omarchy.system-update" => plugins.update.available,
        "omarchy.weather" => plugins.weather.available,
        "omarchy.media" => {
            system.media.available
                && (!system.media.title.is_empty() || !system.media.artist.is_empty())
        }
        "omarchy.tailscale" => plugins.tailscale.installed,
        "omarchy.dropbox" => plugins.dropbox.installed,
        "omarchy.spacer" => {
            entry
                .settings
                .get("size")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(12.0)
                > 0.0
        }
        _ => true,
    }
}

fn label_for_entry(
    entry: &BarEntry,
    clock: &str,
    system: &SystemSnapshot,
    plugins: &PluginSnapshot,
) -> String {
    match entry.id.as_str() {
        "omarchy.clock" => clock.to_string(),
        "omarchy.workspaces" => {
            if system.hyprland.active_workspace.is_empty() {
                "WORKSPACES".to_string()
            } else {
                format!("WS {}", system.hyprland.active_workspace)
            }
        }
        "omarchy.active-window" => {
            if system.hyprland.active_window.is_empty() {
                "DESKTOP".to_string()
            } else {
                truncate(&system.hyprland.active_window, 32)
            }
        }
        "omarchy.audio" => system
            .audio
            .output_percent
            .map(|percent| {
                format!(
                    "VOL {percent}%{}",
                    if system.audio.output_muted {
                        " MUTE"
                    } else {
                        ""
                    }
                )
            })
            .unwrap_or_else(|| "VOL —".to_string()),
        "omarchy.microphone" => system
            .audio
            .input_percent
            .map(|percent| {
                format!(
                    "MIC {percent}%{}",
                    if system.audio.input_muted {
                        " MUTE"
                    } else {
                        ""
                    }
                )
            })
            .unwrap_or_else(|| "MIC —".to_string()),
        "omarchy.network" => {
            if system.network.connection.is_empty() || system.network.connection == "--" {
                "NETWORK OFF".to_string()
            } else {
                format!("NET {}", truncate(&system.network.connection, 18))
            }
        }
        "omarchy.bluetooth" => {
            if !system.bluetooth.available {
                "BT —".to_string()
            } else if system.bluetooth.connected_devices > 0 {
                format!("BT {}", system.bluetooth.connected_devices)
            } else if system.bluetooth.powered {
                "BT ON".to_string()
            } else {
                "BT OFF".to_string()
            }
        }
        "omarchy.power" => system
            .battery
            .percentage
            .map(|percent| format!("PWR {percent}%"))
            .unwrap_or_else(|| "PWR —".to_string()),
        "omarchy.media" => {
            if system.media.title.is_empty() {
                "MEDIA".to_string()
            } else {
                truncate(
                    &format!("{} — {}", system.media.artist, system.media.title),
                    28,
                )
            }
        }
        "omarchy.agents" => {
            if plugins.agents.available {
                format!("AGENT {}", plugins.agents.default_agent.to_uppercase())
            } else {
                "AGENTS".to_string()
            }
        }
        "omarchy.keyboard-layout" => {
            if plugins.keyboard.layout_label.is_empty() {
                "KEYBOARD".to_string()
            } else {
                plugins.keyboard.layout_label.clone()
            }
        }
        "omarchy.weather" => {
            if plugins.weather.status.is_empty() {
                "WEATHER".to_string()
            } else {
                truncate(&plugins.weather.status, 32)
            }
        }
        "omarchy.system-update" => "UPDATE".to_string(),
        "omarchy.dropbox" => {
            if plugins.dropbox.running {
                "DROPBOX".to_string()
            } else {
                "DROPBOX OFF".to_string()
            }
        }
        "omarchy.tailscale" => {
            if plugins.tailscale.running {
                "TAILSCALE".to_string()
            } else if plugins.tailscale.needs_login {
                "TAILSCALE LOGIN".to_string()
            } else {
                "TAILSCALE OFF".to_string()
            }
        }
        _ => label_for(&entry.id).to_string(),
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.push('…');
    }
    output
}

fn label_for(id: &str) -> &str {
    match id {
        "omarchy.menu" => "MENU",
        "omarchy.workspaces" => "WORKSPACES",
        "omarchy.indicators" => "INDICATORS",
        "omarchy.keyboard-layout" => "KEYBOARD",
        "omarchy.weather" => "WEATHER",
        "omarchy.system-update" => "UPDATE",
        "omarchy.tray" => "TRAY",
        "omarchy.agents" => "AGENTS",
        "omarchy.bluetooth" => "BLUETOOTH",
        "omarchy.network" => "NETWORK",
        "omarchy.audio" => "AUDIO",
        "omarchy.monitor" => "MONITOR",
        "omarchy.power" => "POWER",
        other => other.strip_prefix("omarchy.").unwrap_or(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{menu_label, menu_matches_filter};
    use crate::menu::{MenuItem, MenuItemKind};

    #[test]
    fn menu_search_matches_aliases_and_multiple_terms() {
        let item = MenuItem {
            id: "system.lock".to_string(),
            kind: MenuItemKind::Action,
            label: "Lock Screen".to_string(),
            aliases: vec!["secure".to_string()],
            ..Default::default()
        };
        assert!(menu_matches_filter(&item, "secure screen"));
        assert!(menu_matches_filter(&item, ""));
        assert!(!menu_matches_filter(&item, "bluetooth"));
    }

    #[test]
    fn checked_menu_rows_keep_the_reference_mark() {
        let item = MenuItem {
            label: "Performance".to_string(),
            checked: "true".to_string(),
            ..Default::default()
        };
        assert_eq!(menu_label(&item), "Performance  ✓");
    }
}

fn local_clock() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;

    #[cfg(unix)]
    {
        let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
        let result = unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) };
        if !result.is_null() {
            let local = unsafe { local.assume_init() };
            return format!("{:02}:{:02}", local.tm_hour, local.tm_min);
        }
    }

    let minutes = (seconds / 60) % (24 * 60);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}
