use std::{
    collections::BTreeMap,
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
use crate::system::{BluetoothDeviceAction, SystemAction, SystemSnapshot, run_action};

pub struct ShellView {
    snapshot: ShellSnapshot,
    system: SystemSnapshot,
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
            let label = label_for_entry(entry, clock, system);
            let _settings_are_preserved = &entry.settings;
            let id = entry.id.clone();
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
        let panel_payload = payload.to_string();
        let Ok(handle) = cx.open_window(
            WindowOptions {
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: size(px(520.0), px(560.0)),
                })),
                app_id: Some("omarchy-gpui-panel".to_string()),
                window_background: WindowBackgroundAppearance::Opaque,
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "omarchy-gpui-panel".to_string(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::RIGHT,
                    margin: Some((px(54.0), px(12.0), px(12.0), px(12.0))),
                    keyboard_interactivity: KeyboardInteractivity::OnDemand,
                    ..Default::default()
                }),
                focus: true,
                is_movable: false,
                is_resizable: false,
                ..Default::default()
            },
            move |_, cx| cx.new(|_| PanelView::new(panel_id.clone(), panel_state, &panel_payload)),
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
                cx,
            ))
            .child(div().flex_1().flex().justify_center().child(Self::group(
                &self.snapshot.center,
                &self.clock,
                &self.system,
                cx,
            )))
            .child(div().flex().justify_end().child(Self::group(
                &self.snapshot.right,
                &self.clock,
                &self.system,
                cx,
            )))
    }
}

struct PanelView {
    id: String,
    system: SystemSnapshot,
    message: String,
    menu: Option<MenuModel>,
    active_menu: String,
    menu_children: BTreeMap<String, Vec<MenuItem>>,
    filter_text: String,
    selected_menu_index: usize,
}

impl PanelView {
    fn new(id: String, system: SystemSnapshot, payload: &str) -> Self {
        let menu = (id == "omarchy.menu").then(MenuModel::load);
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
            system,
            message: String::new(),
            menu,
            active_menu,
            menu_children: BTreeMap::new(),
            filter_text: String::new(),
            selected_menu_index: 0,
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
            _ => {}
        }
        actions
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
        } else {
            let mut rows = div().flex().flex_col().gap_2().mt_3();
            for (label, value) in panel_rows(&self.id, &self.system) {
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
        let actions = self.actions(cx);

        div()
            .id("omarchy-gpui-panel")
            .size_full()
            .flex()
            .flex_col()
            .p_5()
            .bg(rgb(0x18181b))
            .text_color(rgb(0xf4f4f5))
            .on_key_down(cx.listener(Self::handle_key))
            .child(
                div()
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
                    ),
            )
            .child(content)
            .child(actions)
            .when(!self.message.is_empty(), |panel| {
                panel.child(
                    div()
                        .mt_3()
                        .text_color(rgb(0xa1a1aa))
                        .child(self.message.clone()),
                )
            })
    }
}

fn is_panel_capable(id: &str) -> bool {
    matches!(
        id,
        "omarchy.agents"
            | "omarchy.audio"
            | "omarchy.bluetooth"
            | "omarchy.clock"
            | "omarchy.dropbox"
            | "omarchy.menu"
            | "omarchy.monitor"
            | "omarchy.network"
            | "omarchy.power"
            | "omarchy.tailscale"
            | "omarchy.weather"
            | "omarchy.media"
    )
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

fn panel_rows(id: &str, system: &SystemSnapshot) -> Vec<(String, String)> {
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

fn label_for_entry(entry: &BarEntry, clock: &str, system: &SystemSnapshot) -> String {
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
