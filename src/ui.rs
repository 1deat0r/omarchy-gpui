use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{
    AppContext, Bounds, ClickEvent, Context, Div, KeyDownEvent, ObjectFit, Render, ScrollDelta,
    ScrollWheelEvent, Stateful, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, div, img, layer_shell::*, point, prelude::*, px, rgb, rgba, size,
};

use crate::config::{BarEntry, ShellSnapshot};
use crate::dbus::{TrayAction, TrayItem};
use crate::ipc::{IpcEvent, IpcEventReceiver};
use crate::menu::{DmenuOption, DmenuRequest, MenuItem, MenuItemKind, MenuModel};
use crate::overlays::{
    OverlayAction, OverlayRow, clipboard_rows_from_path, default_clipboard_history_path,
    emoji_rows_from_path, image_rows_from_payload, parse_image_picker_payload, reminder_args,
    valid_reminder_minutes,
};
use crate::plugins::{IndicatorState, PluginSnapshot, WeatherDay, parse_network_qr};
use crate::system::{
    BluetoothDeviceAction, SystemAction, SystemSnapshot, run_action, subscribe_hyprland_events,
};

pub struct ShellView {
    snapshot: ShellSnapshot,
    system: SystemSnapshot,
    plugins: PluginSnapshot,
    clock: String,
    smoke: bool,
    reported_first_frame: bool,
    background_window: Option<WindowHandle<BackgroundView>>,
    panel_id: Option<String>,
    panel_window: Option<WindowHandle<PanelView>>,
    notification_window: Option<WindowHandle<NotificationPopupView>>,
    notification_generation: u64,
}

impl ShellView {
    pub fn new(
        snapshot: ShellSnapshot,
        smoke: bool,
        ipc_events: IpcEventReceiver,
        background_window: Option<WindowHandle<BackgroundView>>,
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

        if let Some(receiver) = subscribe_hyprland_events() {
            let events = Arc::new(std::sync::Mutex::new(receiver));
            cx.spawn(async move |this, cx| {
                loop {
                    let changed = events
                        .lock()
                        .map(|receiver| receiver.try_iter().next().is_some())
                        .unwrap_or(false);
                    if changed {
                        let system = cx
                            .background_executor()
                            .spawn(async { SystemSnapshot::collect() })
                            .await;
                        if this
                            .update(cx, |view, cx| {
                                view.system = system;
                                view.clock = local_clock();
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(40))
                        .await;
                }
            })
            .detach();
        }

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
            background_window,
            panel_id: None,
            panel_window: None,
            notification_window: None,
            notification_generation: 0,
        }
    }

    fn group(
        snapshot: &ShellSnapshot,
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
            if entry.id == "omarchy.indicators" {
                let indicators = indicator_bar_items(&plugins.indicators, system);
                for (indicator_id, label, active) in indicators {
                    if !active {
                        continue;
                    }
                    let click_id = indicator_id.clone();
                    group = group.child(
                        Self::chip(&label, &format!("omarchy-indicator-{indicator_id}"))
                            .cursor_pointer()
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.handle_indicator_click(&click_id, window, cx);
                            })),
                    );
                }
                continue;
            }
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
            if entry.id == "omarchy.tray" {
                for item in &plugins.tray.items {
                    if tray_item_owned_by_omarchy(item, snapshot) {
                        continue;
                    }
                    let item = item.clone();
                    let label = tray_item_label(&item);
                    let id = format!("omarchy-tray-{}", sanitize_id(&item.id));
                    let scroll_item = item.clone();
                    group = group.child(
                        Self::chip(&label, &id)
                            .cursor_pointer()
                            .on_click(cx.listener(move |view, event, _window, cx| {
                                view.handle_tray_click(&item, event, cx);
                            }))
                            .on_scroll_wheel(cx.listener(
                                move |_view, event: &ScrollWheelEvent, _window, _cx| {
                                    let delta = match event.delta {
                                        ScrollDelta::Pixels(value) => f32::from(value.y),
                                        ScrollDelta::Lines(value) => value.y,
                                    };
                                    if delta != 0.0 {
                                        let _ = crate::dbus::tray_action(
                                            &scroll_item,
                                            TrayAction::Scroll {
                                                delta: delta.signum() as i32,
                                                orientation: "vertical".to_string(),
                                            },
                                        );
                                    }
                                },
                            )),
                    );
                }
                continue;
            }
            if !entry_visible(entry, plugins, system) {
                continue;
            }
            let label = label_for_entry(
                entry,
                clock,
                matches!(snapshot.bar_position.as_str(), "left" | "right"),
                system,
                plugins,
            );
            let _settings_are_preserved = &entry.settings;
            let id = entry.id.clone();
            if id == "omarchy.media" {
                let click_id = id.clone();
                group = group.child(
                    Self::chip(&label, &entry.id)
                        .cursor_pointer()
                        .on_click(cx.listener(move |view, event, window, cx| {
                            view.handle_bar_click(&click_id, event, window, cx);
                        }))
                        .on_scroll_wheel(cx.listener(
                            |_view, event: &ScrollWheelEvent, _window, _cx| {
                                let delta = match event.delta {
                                    ScrollDelta::Pixels(value) => f32::from(value.y),
                                    ScrollDelta::Lines(value) => value.y,
                                };
                                if delta > 0.0 {
                                    let _ = run_action(&SystemAction::MediaPrevious);
                                } else if delta < 0.0 {
                                    let _ = run_action(&SystemAction::MediaNext);
                                }
                            },
                        )),
                );
                continue;
            }
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
            let clickable = is_panel_capable(&id) || is_bar_actionable(&id);
            group = group.child(Self::chip(&label, &entry.id).when(clickable, |chip| {
                chip.cursor_pointer()
                    .on_click(cx.listener(move |view, event, window, cx| {
                        view.handle_bar_click(&id, event, window, cx);
                    }))
            }));
        }
        group
    }

    fn handle_indicator_click(
        &mut self,
        indicator: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match indicator {
            "Dictation" => {
                let _ = Command::new("omarchy-voxtype-config").spawn();
            }
            "ScreenRecording" => {
                if self.plugins.indicators.recording {
                    let _ = Command::new("omarchy-capture-screenrecording")
                        .arg("--stop-recording")
                        .spawn();
                } else {
                    let _ = Command::new("omarchy-menu")
                        .args(["toggle", "trigger.capture.screenrecord"])
                        .spawn();
                }
            }
            "Reminder" => {
                self.open_panel("omarchy.reminders", "{}", cx);
            }
            "NightLight" => {
                let _ = run_action(&SystemAction::SetNightlight(!self.system.nightlight.active));
            }
            "Dnd" => {
                let executable =
                    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("omarchy-gpui-shell"));
                let _ = Command::new(executable)
                    .args(["notifications", "toggleDnd"])
                    .spawn();
            }
            "StayAwake" => {
                let _ = Command::new("omarchy-toggle-idle").arg("toggle").spawn();
            }
            _ => {}
        }
        let _ = window;
    }

    fn handle_bar_click(
        &mut self,
        id: &str,
        event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match id {
            "omarchy.active-window" => {
                let address = self.system.hyprland.active_address.clone();
                if address.is_empty() {
                    return;
                }
                let action = if event.is_middle_click() || event.is_right_click() {
                    "closewindow"
                } else {
                    "focuswindow"
                };
                let _ = Command::new("hyprctl")
                    .args(["dispatch", action, &format!("address:{address}")])
                    .spawn();
            }
            "omarchy.keyboard-layout" => {
                let keyboard = self.plugins.keyboard.keyboard_name.clone();
                if !keyboard.is_empty() {
                    let _ = Command::new("hyprctl")
                        .args(["switchxkblayout", &keyboard, "next"])
                        .spawn();
                }
            }
            "omarchy.microphone" => {
                let _ = run_action(&SystemAction::ToggleInputMute);
                cx.notify();
            }
            "omarchy.system-update" => {
                let _ = Command::new("omarchy-launch-floating-terminal-with-presentation")
                    .arg("omarchy-update")
                    .spawn();
            }
            "omarchy.clock" => {
                if event.is_right_click() {
                    let executable = std::env::current_exe()
                        .unwrap_or_else(|_| PathBuf::from("omarchy-gpui-shell"));
                    let _ = Command::new(executable)
                        .args(["shell", "call", "clock", "cycleFormat"])
                        .spawn();
                } else if event.is_middle_click() {
                    self.spawn_omarchy_command("omarchy-menu-timezone", &[]);
                } else {
                    self.toggle_panel(id, cx);
                }
            }
            "omarchy.weather" => {
                if event.is_right_click() {
                    let status = self
                        .omarchy_command("omarchy-weather-status", &[])
                        .output()
                        .ok()
                        .filter(|output| output.status.success())
                        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                        .filter(|status| !status.is_empty());
                    if let Some(status) = status {
                        self.spawn_omarchy_command("omarchy-notification-send", &[&status]);
                    }
                } else if event.is_middle_click() {
                    let executable = std::env::current_exe()
                        .unwrap_or_else(|_| PathBuf::from("omarchy-gpui-shell"));
                    let _ = Command::new(executable)
                        .args(["shell", "call", "weather", "refresh"])
                        .spawn();
                } else {
                    self.toggle_panel(id, cx);
                }
            }
            "omarchy.media" => {
                if event.is_right_click() {
                    self.toggle_panel(id, cx);
                } else if event.is_middle_click() {
                    let _ = run_action(&SystemAction::MediaNext);
                } else {
                    let _ = run_action(&SystemAction::MediaPlayPause);
                }
                cx.notify();
            }
            _ if is_panel_capable(id) => self.toggle_panel(id, cx),
            _ => {}
        }
    }

    fn omarchy_command(&self, command: &str, args: &[&str]) -> Command {
        let bundled = self.snapshot.omarchy_path.join("bin").join(command);
        let program = if bundled.is_file() {
            bundled
        } else {
            PathBuf::from(command)
        };
        let mut process = Command::new(program);
        process.args(args);
        process
    }

    fn spawn_omarchy_command(&self, command: &str, args: &[&str]) {
        let _ = self.omarchy_command(command, args).spawn();
    }

    fn handle_tray_click(&mut self, item: &TrayItem, event: &ClickEvent, cx: &mut Context<Self>) {
        let action = if event.is_right_click() {
            TrayAction::ContextMenu
        } else if event.is_middle_click() {
            TrayAction::SecondaryActivate
        } else if item.item_is_menu || !item.menu_path.is_empty() {
            TrayAction::ContextMenu
        } else {
            TrayAction::Activate
        };
        let result = crate::dbus::tray_action(item, action);
        if let Err(error) = result {
            eprintln!("omarchy-gpui-shell: tray action: {error}");
        }
        cx.notify();
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
        let dmenu_request = (panel_id == "omarchy.menu")
            .then(|| DmenuRequest::parse(&panel_payload))
            .flatten();
        let panel_width = dmenu_request
            .as_ref()
            .map(|request| request.width as f32)
            .unwrap_or(520.0);
        let panel_height = dmenu_request
            .as_ref()
            .and_then(|request| {
                (request.max_height > 0).then_some(request.max_height as f32 + 140.0)
            })
            .unwrap_or(560.0)
            .clamp(240.0, 800.0);
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
                        size(px(panel_width), px(panel_height))
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

    fn close_notification(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.notification_window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn show_notification(&mut self, payload: &str, cx: &mut Context<Self>) {
        let Some(entry) = parse_notification_history(&format!("[{payload}]"))
            .into_iter()
            .next()
        else {
            return;
        };
        self.close_notification(cx);
        self.notification_generation = self.notification_generation.wrapping_add(1);
        let generation = self.notification_generation;
        let lifetime = notification_lifetime(&entry);
        let popup_entry = entry.clone();
        let Ok(handle) = cx.open_window(
            WindowOptions {
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: size(px(430.0), px(180.0)),
                })),
                app_id: Some("omarchy-gpui-notification".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "omarchy-gpui-notifications".to_string(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::RIGHT,
                    margin: Some((px(54.0), px(12.0), px(12.0), px(12.0))),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                focus: false,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                ..Default::default()
            },
            move |_, cx| cx.new(|_| NotificationPopupView::new(popup_entry)),
        ) else {
            return;
        };
        self.notification_window = Some(handle);
        if let Some(lifetime) = lifetime {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(lifetime).await;
                let _ = this.update(cx, |view, cx| {
                    if view.notification_generation == generation {
                        view.close_notification(cx);
                        let executable = std::env::current_exe()
                            .unwrap_or_else(|_| PathBuf::from("omarchy-gpui-shell"));
                        let _ = Command::new(executable)
                            .args(["shell", "call", "notifications", "dismissOne"])
                            .spawn();
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn apply_ipc_event(&mut self, event: IpcEvent, cx: &mut Context<Self>) {
        match event {
            IpcEvent::Refresh => {}
            IpcEvent::Background { path, .. } => {
                if let Some(handle) = self.background_window.as_ref() {
                    let _ = handle.update(cx, |view, _, cx| {
                        view.path = PathBuf::from(path);
                        cx.notify();
                    });
                }
            }
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
            IpcEvent::Lock { .. } => {}
            IpcEvent::Notification { entry } => {
                self.show_notification(&entry, cx);
            }
            IpcEvent::NotificationHistory { entries } => {
                self.close_panel(cx);
                self.open_panel("omarchy.notifications", &entries, cx);
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
            .child(Self::group(
                &self.snapshot,
                &self.snapshot.left,
                &self.clock,
                &self.system,
                &self.plugins,
                cx,
            ))
            .child(div().flex_1().flex().justify_center().child(Self::group(
                &self.snapshot,
                &self.snapshot.center,
                &self.clock,
                &self.system,
                &self.plugins,
                cx,
            )))
            .child(div().flex().justify_end().child(Self::group(
                &self.snapshot,
                &self.snapshot.right,
                &self.clock,
                &self.system,
                &self.plugins,
                cx,
            )))
    }
}

struct NotificationPopupView {
    entry: NotificationEntry,
}

impl NotificationPopupView {
    fn new(entry: NotificationEntry) -> Self {
        Self { entry }
    }
}

impl Render for NotificationPopupView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let heading = if self.entry.app.is_empty() {
            self.entry.summary.clone()
        } else if self.entry.summary.is_empty() {
            self.entry.app.clone()
        } else {
            format!("{} · {}", self.entry.app, self.entry.summary)
        };
        let badge = if self.entry.glyph.is_empty() {
            self.entry
                .app_icon
                .chars()
                .next()
                .or_else(|| self.entry.app.chars().next())
                .unwrap_or('•')
                .to_string()
        } else {
            self.entry.glyph.clone()
        };
        let has_open_action = !self.entry.exec_argv.is_empty() || !self.entry.app.is_empty();
        let mut footer = div().flex().justify_end().gap_2().mt_3();
        if has_open_action {
            footer = footer.child(
                div()
                    .id("omarchy-gpui-notification-open")
                    .cursor_pointer()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x3f3f46))
                    .child("OPEN")
                    .on_click(|_, window, _| {
                        let executable = std::env::current_exe()
                            .unwrap_or_else(|_| PathBuf::from("omarchy-gpui-shell"));
                        let _ = Command::new(executable)
                            .args(["shell", "call", "notifications", "invokeLast"])
                            .spawn();
                        window.remove_window();
                    }),
            );
        }
        footer = footer.child(
            div()
                .id("omarchy-gpui-notification-dismiss")
                .cursor_pointer()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(0x27272a))
                .child("DISMISS")
                .on_click(|_, window, _| {
                    let executable = std::env::current_exe()
                        .unwrap_or_else(|_| PathBuf::from("omarchy-gpui-shell"));
                    let _ = Command::new(executable)
                        .args(["shell", "call", "notifications", "dismissOne"])
                        .spawn();
                    window.remove_window();
                }),
        );

        div()
            .id(format!("omarchy-gpui-notification-popup-{}", self.entry.id))
            .size_full()
            .p_3()
            .rounded_lg()
            .bg(rgba(0x18181bf5))
            .border_1()
            .border_color(rgb(0x52525b))
            .text_color(rgb(0xf4f4f5))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(28.0))
                            .h(px(28.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(rgb(0x3f3f46))
                            .child(badge),
                    )
                    .child(div().flex_1().text_size(px(14.0)).child(heading)),
            )
            .when(!self.entry.body.is_empty(), |popup| {
                popup.child(
                    div()
                        .mt_2()
                        .text_size(px(12.0))
                        .text_color(rgb(0xd4d4d8))
                        .child(self.entry.body.clone()),
                )
            })
            .when(!self.entry.image.is_empty(), |popup| {
                popup.child(
                    div()
                        .mt_1()
                        .text_size(px(10.0))
                        .text_color(rgb(0xa1a1aa))
                        .child("Image attachment"),
                )
            })
            .child(footer)
    }
}

pub struct BackgroundView {
    path: PathBuf,
}

impl BackgroundView {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Render for BackgroundView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let path = self.path.clone();
        div()
            .id("omarchy-gpui-background")
            .size_full()
            .bg(rgb(0x000000))
            .when(!path.as_os_str().is_empty(), |background| {
                background.child(
                    img(path)
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .with_fallback(|| div().size_full().bg(rgb(0x000000)).into_any_element()),
                )
            })
    }
}

pub fn current_background_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    fs::canonicalize(home.join(".local/state/omarchy/current/background")).unwrap_or_default()
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
    dmenu: Option<DmenuRequest>,
    qr_meta: String,
    qr_lines: Vec<String>,
    osd_icon: String,
    osd_message: String,
    notification_entries: Vec<NotificationEntry>,
    reminder_minutes: String,
    reminder_step_message: bool,
    weather_editing: bool,
    calendar_year: i32,
    calendar_month: u8,
    calendar_today: (i32, u8, u8),
}

#[derive(Clone, Debug, Default)]
struct NotificationEntry {
    id: u32,
    app: String,
    app_icon: String,
    summary: String,
    body: String,
    image: String,
    glyph: String,
    exec_argv: String,
    urgency: u8,
    expire_timeout: u32,
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
        let dmenu = (id == "omarchy.menu")
            .then(|| DmenuRequest::parse(payload))
            .flatten();
        let (overlay_rows, overlay_filterable) = overlay_rows_for(&id, payload, &snapshot);
        let (qr_meta, qr_lines) = if id == "omarchy.wifiqr" {
            wifi_qr_payload(&snapshot)
        } else {
            (String::new(), Vec::new())
        };
        let (osd_icon, osd_message) = parse_osd_payload(payload);
        let notification_entries = parse_notification_history(payload);
        let calendar_today = local_date_parts();
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
        let weather_editing = id == "omarchy.weather"
            && serde_json::from_str::<serde_json::Value>(payload)
                .ok()
                .and_then(|value| value.get("edit").and_then(serde_json::Value::as_bool))
                .unwrap_or(false);
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
            dmenu,
            qr_meta,
            qr_lines,
            osd_icon,
            osd_message,
            notification_entries,
            reminder_minutes: String::new(),
            reminder_step_message: false,
            weather_editing,
            calendar_year: calendar_today.0,
            calendar_month: calendar_today.1,
            calendar_today,
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
        let mut actions = div().flex().flex_wrap().gap_2().mt_4();
        match self.id.as_str() {
            "omarchy.audio" => {
                actions = actions
                    .child(self.action_button("Mute output", SystemAction::ToggleOutputMute, cx))
                    .child(self.action_button("Mute input", SystemAction::ToggleInputMute, cx))
                    .child(self.action_button("Set 50%", SystemAction::SetOutputVolume(50), cx));
                for node in &self.system.audio.sinks {
                    let name = truncate(&node.description, 22);
                    let node_id = node.id;
                    let current = node
                        .volume
                        .map_or_else(|| "—".to_string(), |volume| format!("{volume}%"));
                    actions = actions
                        .child(self.action_button(
                            &format!("Output {name} {current}"),
                            SystemAction::SetDefaultAudioSink {
                                id: node_id,
                                name: node.name.clone(),
                            },
                            cx,
                        ))
                        .child(self.action_button(
                            &format!("Output {name} +10"),
                            SystemAction::SetAudioNodeVolume {
                                id: node_id,
                                percent: node.volume.unwrap_or(50).saturating_add(10).min(100),
                            },
                            cx,
                        ))
                        .child(self.action_button(
                            &format!("Output {name} -10"),
                            SystemAction::SetAudioNodeVolume {
                                id: node_id,
                                percent: node.volume.unwrap_or(50).saturating_sub(10),
                            },
                            cx,
                        ))
                        .child(self.action_button(
                            &format!("{} {}", if node.muted { "Unmute" } else { "Mute" }, name),
                            SystemAction::ToggleAudioNodeMute { id: node_id },
                            cx,
                        ));
                }
                for node in &self.system.audio.sources {
                    let name = truncate(&node.description, 22);
                    actions = actions.child(self.action_button(
                        &format!("Input {name}"),
                        SystemAction::SetDefaultAudioSource {
                            id: node.id,
                            name: node.name.clone(),
                        },
                        cx,
                    ));
                }
                for node in &self.system.audio.streams {
                    if node.volume.is_none() {
                        continue;
                    }
                    let name = truncate(
                        if node.application.is_empty() {
                            &node.description
                        } else {
                            &node.application
                        },
                        22,
                    );
                    actions = actions
                        .child(self.action_button(
                            &format!("Stream {name} +10"),
                            SystemAction::SetAudioNodeVolume {
                                id: node.id,
                                percent: node.volume.unwrap_or(50).saturating_add(10).min(100),
                            },
                            cx,
                        ))
                        .child(self.action_button(
                            &format!("Stream {name} -10"),
                            SystemAction::SetAudioNodeVolume {
                                id: node.id,
                                percent: node.volume.unwrap_or(50).saturating_sub(10),
                            },
                            cx,
                        ));
                }
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
                actions = actions
                    .child(self.ipc_button("Source previous", &["media", "sourcePrevious"], cx))
                    .child(self.ipc_button("Source next", &["media", "sourceNext"], cx));
            }
            "omarchy.clock" => {
                actions = actions
                    .child(self.ipc_button("Cycle format", &["clock", "cycleFormat"], cx))
                    .child(self.ipc_button("Toggle week start", &["clock", "toggleWeekStart"], cx));
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
                        SystemAction::SetNightlight(!self.system.nightlight.active),
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
                            enabled: !display.enabled,
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
                actions = actions.child(self.ipc_button(
                    "Toggle percentage",
                    &["power", "togglePercentage"],
                    cx,
                ));
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
            "omarchy.notifications" => {
                actions = actions
                    .child(self.ipc_button("Clear history", &["notifications", "clear"], cx))
                    .child(self.ipc_button("Dismiss all", &["notifications", "dismissAll"], cx));
            }
            "omarchy.disk-speedtest" => {
                actions = actions.child(self.command_button(
                    "Run disk test",
                    "omarchy-disk-speedtest",
                    &[],
                    cx,
                ));
            }
            "omarchy.speedtest" => {
                actions = actions
                    .child(self.command_button(
                        "Download test",
                        "omarchy-network-speedtest",
                        &["down"],
                        cx,
                    ))
                    .child(self.command_button(
                        "Upload test",
                        "omarchy-network-speedtest",
                        &["up"],
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
                    ))
                    .child(self.ipc_button("Edit location", &["weather", "edit"], cx));
            }
            _ => {}
        }
        actions
    }

    fn ipc_button(&self, label: &str, args: &[&str], cx: &mut Context<Self>) -> Stateful<Div> {
        let id = format!(
            "omarchy-gpui-ipc-{}",
            label.to_lowercase().replace(' ', "-")
        );
        let executable =
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("omarchy-gpui-shell"));
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
                view.message = match Command::new(&executable).args(&args).spawn() {
                    Ok(_) => "IPC action started".to_string(),
                    Err(error) => format!("{}: {error}", executable.display()),
                };
                cx.notify();
            }))
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

    fn visible_dmenu_options(&mut self) -> Vec<(usize, DmenuOption)> {
        let Some(request) = self.dmenu.as_ref() else {
            return Vec::new();
        };
        if request.input {
            return Vec::new();
        }
        let query = self.filter_text.trim().to_lowercase();
        let options = request
            .options
            .iter()
            .enumerate()
            .filter(|(_, option)| {
                query.is_empty()
                    || format!("{} {}", option.label, option.detail)
                        .to_lowercase()
                        .contains(&query)
            })
            .map(|(index, option)| (index, option.clone()))
            .collect::<Vec<_>>();
        if options.is_empty() {
            self.selected_menu_index = 0;
        } else if self.selected_menu_index >= options.len() {
            self.selected_menu_index = options.len() - 1;
        }
        options
    }

    fn dmenu_content(&mut self, cx: &mut Context<Self>) -> Div {
        let Some(request) = self.dmenu.clone() else {
            return div();
        };
        let mut content = div().flex().flex_col().gap_1().mt_3();
        content = content.child(
            div()
                .id("omarchy-gpui-dmenu-prompt")
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
                    format!("{}…", request.prompt)
                } else {
                    self.filter_text.clone()
                }),
        );
        if request.input {
            return content.child(
                div()
                    .text_color(rgb(0xa1a1aa))
                    .child("Press Enter to submit, or Esc to cancel."),
            );
        }

        let options = self.visible_dmenu_options();
        let options_empty = options.is_empty();
        let mut rows = div().flex().flex_col().gap_1();
        for (visible_index, (index, option)) in options.into_iter().enumerate() {
            let label = if option.icon.is_empty() {
                option.label.clone()
            } else {
                format!("{}  {}", option.icon, option.label)
            };
            let detail = option.detail.clone();
            rows = rows.child(
                div()
                    .id(format!("omarchy-gpui-dmenu-item-{index}"))
                    .cursor_pointer()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(if visible_index == self.selected_menu_index {
                        rgb(0x3f3f46)
                    } else {
                        rgb(0x27272a)
                    })
                    .border_1()
                    .border_color(rgb(0x3f3f46))
                    .child(label)
                    .when(!detail.is_empty(), |row| {
                        row.child(
                            div()
                                .mt_1()
                                .text_size(px(11.0))
                                .text_color(rgb(0xa1a1aa))
                                .child(detail.clone()),
                        )
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.finish_dmenu(Some(index), window, cx);
                    })),
            );
        }
        if options_empty {
            rows = rows.child(
                div()
                    .text_color(rgb(0xa1a1aa))
                    .child("No options available."),
            );
        }
        content.child(rows)
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
        if self.dmenu.is_some() {
            self.handle_dmenu_key(event, window, cx);
            return;
        }
        if self.id == "omarchy.weather" {
            self.handle_weather_key(event, window, cx);
            return;
        }
        if self.id == "omarchy.clock" {
            self.handle_clock_key(event, window, cx);
            return;
        }
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

    fn handle_dmenu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => {
                self.finish_dmenu(None, window, cx);
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
                let count = self.visible_dmenu_options().len();
                if count > 0 {
                    self.selected_menu_index = self.selected_menu_index.saturating_sub(1);
                }
                cx.notify();
                return;
            }
            "down" | "j" => {
                let count = self.visible_dmenu_options().len();
                if count > 0 {
                    self.selected_menu_index =
                        (self.selected_menu_index + 1).min(count.saturating_sub(1));
                }
                cx.notify();
                return;
            }
            "enter" | "return" => {
                let request = self.dmenu.clone();
                if let Some(request) = request {
                    if request.input {
                        self.finish_dmenu_value(
                            request.result_for(None, &self.filter_text),
                            window,
                            cx,
                        );
                    } else if let Some((_, option)) = self
                        .visible_dmenu_options()
                        .get(self.selected_menu_index)
                        .cloned()
                    {
                        self.finish_dmenu_value(request.result_for(Some(&option), ""), window, cx);
                    }
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

    fn finish_dmenu(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let value = self
            .dmenu
            .as_ref()
            .and_then(|request| index.and_then(|index| request.options.get(index)))
            .map(DmenuOption::selection_value);
        self.finish_dmenu_value(value.unwrap_or_default(), window, cx);
    }

    fn finish_dmenu_value(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.dmenu.clone() else {
            window.remove_window();
            return;
        };
        let result = if request.done_file.trim().is_empty() {
            Ok(())
        } else if value.is_empty() {
            truncate_file(&request.done_file)
        } else if request.selection_file.trim().is_empty() {
            truncate_file(&request.done_file)
        } else {
            write_text_file(&request.selection_file, &format!("{value}\n"))
                .and_then(|()| truncate_file(&request.done_file))
        };
        match result {
            Ok(()) => window.remove_window(),
            Err(error) => self.message = error,
        }
        cx.notify();
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

impl PanelView {
    fn notification_content(&self) -> Div {
        let mut rows = div().flex().flex_col().gap_2().mt_3();
        if self.notification_entries.is_empty() {
            return rows.child(
                div()
                    .text_color(rgb(0xa1a1aa))
                    .child("No recent notifications"),
            );
        }
        for entry in &self.notification_entries {
            let heading = if entry.app.is_empty() {
                entry.summary.clone()
            } else if entry.summary.is_empty() {
                entry.app.clone()
            } else {
                format!("{} · {}", entry.app, entry.summary)
            };
            rows = rows.child(
                div()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x27272a))
                    .border_1()
                    .border_color(rgb(0x3f3f46))
                    .child(heading)
                    .when(!entry.body.is_empty(), |row| {
                        row.child(
                            div()
                                .mt_1()
                                .text_size(px(11.0))
                                .text_color(rgb(0xa1a1aa))
                                .child(entry.body.clone()),
                        )
                    }),
            );
        }
        rows
    }

    fn osd_content(&self) -> Div {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .mt_3()
            .child(
                div()
                    .text_size(px(36.0))
                    .child(if self.osd_icon.is_empty() {
                        "󰎆".to_string()
                    } else {
                        self.osd_icon.clone()
                    }),
            )
            .child(
                div()
                    .text_size(px(18.0))
                    .child(if self.osd_message.is_empty() {
                        "OSD".to_string()
                    } else {
                        self.osd_message.clone()
                    }),
            )
    }

    fn start_weather_editing(&mut self, cx: &mut Context<Self>) {
        self.weather_editing = true;
        self.filter_text = self.plugins.weather.location.clone();
        cx.notify();
    }

    fn handle_weather_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        if !self.weather_editing {
            match key {
                "escape" => window.remove_window(),
                "enter" | "return" => self.start_weather_editing(cx),
                _ => {}
            }
            return;
        }

        match key {
            "escape" => {
                self.weather_editing = false;
                self.filter_text.clear();
                cx.notify();
            }
            "backspace" => {
                self.filter_text.pop();
                cx.notify();
            }
            "enter" | "return" => self.commit_weather_location(cx),
            _ if event.keystroke.modifiers.control
                || event.keystroke.modifiers.alt
                || event.keystroke.modifiers.platform => {}
            _ => {
                if let Some(character) = event
                    .keystroke
                    .key_char
                    .as_deref()
                    .or_else(|| (!key.is_empty() && key.chars().count() == 1).then_some(key))
                    && !character.chars().any(char::is_control)
                {
                    self.filter_text.push_str(character);
                    cx.notify();
                }
            }
        }
    }

    fn commit_weather_location(&mut self, cx: &mut Context<Self>) {
        let location = self.filter_text.trim().to_string();
        let program = self.omarchy_program("omarchy-weather-location");
        let result = if location.is_empty() {
            Command::new(&program).arg("--clear").spawn()
        } else {
            Command::new(&program)
                .args(["--set", location.as_str()])
                .spawn()
        };
        self.message = match result {
            Ok(_) => "Weather location update started".to_string(),
            Err(error) => format!("{}: {error}", program.display()),
        };
        self.weather_editing = false;
        self.filter_text.clear();
        cx.notify();
    }

    fn handle_clock_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => window.remove_window(),
            "left" | "h" => self.step_calendar(-1, 0, cx),
            "right" | "l" => self.step_calendar(1, 0, cx),
            "up" | "k" => self.step_calendar(0, -1, cx),
            "down" | "j" => self.step_calendar(0, 1, cx),
            "home" => {
                self.calendar_year = self.calendar_today.0;
                self.calendar_month = self.calendar_today.1;
                cx.notify();
            }
            _ => {}
        }
    }

    fn step_calendar(&mut self, month_delta: i32, year_delta: i32, cx: &mut Context<Self>) {
        let total = self.calendar_year.saturating_mul(12)
            + i32::from(self.calendar_month.saturating_sub(1))
            + month_delta
            + year_delta.saturating_mul(12);
        self.calendar_year = total.div_euclid(12);
        self.calendar_month = u8::try_from(total.rem_euclid(12) + 1).unwrap_or(1);
        cx.notify();
    }

    fn clock_calendar_content(&mut self, cx: &mut Context<Self>) -> Div {
        let mut content = div().flex().flex_col().gap_3().mt_3();
        let title = format!(
            "{} {}",
            calendar_month_name(self.calendar_month),
            self.calendar_year
        );
        content = content.child(panel_hero("Clock", title)).child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(self.calendar_button("‹", -1, 0, cx))
                .child(
                    div()
                        .id("omarchy-gpui-calendar-today")
                        .cursor_pointer()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(0x27272a))
                        .child("TODAY")
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.calendar_year = view.calendar_today.0;
                            view.calendar_month = view.calendar_today.1;
                            cx.notify();
                        })),
                )
                .child(self.calendar_button("›", 1, 0, cx)),
        );

        let mut weekdays = div().flex().gap_1();
        for weekday in ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"] {
            weekdays = weekdays.child(
                div()
                    .w(px(98.0))
                    .text_size(px(10.0))
                    .text_color(rgb(0xa1a1aa))
                    .child(weekday),
            );
        }
        content = content.child(weekdays);

        let leading =
            calendar_weekday(self.calendar_year, self.calendar_month).saturating_sub(1) as usize;
        let days = calendar_days_in_month(self.calendar_year, self.calendar_month);
        let mut grid = div().flex().flex_wrap().gap_1();
        for index in 0..42 {
            let day = if index >= leading && index < leading + days as usize {
                Some((index - leading + 1) as u8)
            } else {
                None
            };
            let today = day.is_some_and(|day| {
                (self.calendar_year, self.calendar_month, day) == self.calendar_today
            });
            grid = grid.child(
                div()
                    .w(px(98.0))
                    .h(px(34.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(if today { rgb(0x7c3aed) } else { rgb(0x27272a) })
                    .text_color(if day.is_some() {
                        rgb(0xf4f4f5)
                    } else {
                        rgb(0x52525b)
                    })
                    .child(day.map_or_else(String::new, |day| day.to_string())),
            );
        }
        content.child(grid)
    }

    fn calendar_button(
        &self,
        label: &str,
        month_delta: i32,
        year_delta: i32,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let direction = if month_delta < 0 { "previous" } else { "next" };
        div()
            .id(format!("omarchy-gpui-calendar-{direction}"))
            .cursor_pointer()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(rgb(0x27272a))
            .child(label.to_string())
            .on_click(cx.listener(move |view, _, _, cx| {
                view.step_calendar(month_delta, year_delta, cx);
            }))
    }

    fn rich_panel_content(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let mut content = div().flex().flex_col().gap_3().mt_3();
        match self.id.as_str() {
            "omarchy.clock" => Some(self.clock_calendar_content(cx)),
            "omarchy.weather" => {
                let weather = &self.plugins.weather;
                let temperature = if weather.temp_c.is_empty() {
                    "—".to_string()
                } else {
                    format!("{}°C", weather.temp_c)
                };
                let mut hero = div().flex().items_center().gap_3();
                hero = hero
                    .child(
                        div()
                            .w(px(56.0))
                            .h(px(56.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .bg(rgb(0x27272a))
                            .text_size(px(34.0))
                            .child(if weather.icon.is_empty() {
                                "".to_string()
                            } else {
                                weather.icon.clone()
                            }),
                    )
                    .child(panel_hero("Weather", temperature));
                content = content.child(hero);

                if self.weather_editing {
                    content = content.child(
                        div()
                            .id("omarchy-gpui-weather-location-input")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(0x27272a))
                            .border_1()
                            .border_color(rgb(0x7c3aed))
                            .text_color(if self.filter_text.is_empty() {
                                rgb(0x71717a)
                            } else {
                                rgb(0xf4f4f5)
                            })
                            .child(if self.filter_text.is_empty() {
                                "Type a city and press Enter…".to_string()
                            } else {
                                self.filter_text.clone()
                            }),
                    );
                } else {
                    let location = if weather.location.is_empty() {
                        "Auto-detected location".to_string()
                    } else {
                        weather.location.to_uppercase()
                    };
                    content = content.child(
                        div()
                            .id("omarchy-gpui-weather-location")
                            .cursor_pointer()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(0x27272a))
                            .child(format!("⌖ {location}"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.start_weather_editing(cx);
                            })),
                    );
                }

                if !weather.condition.is_empty() {
                    content = content.child(panel_text_row("Condition", &weather.condition, false));
                }
                let feels = if weather.feels_c.is_empty() {
                    "—".to_string()
                } else {
                    format!("{}°C", weather.feels_c)
                };
                let wind = if weather.wind_kmph.is_empty() {
                    "—".to_string()
                } else {
                    format!("{} km/h", weather.wind_kmph)
                };
                let humidity = if weather.humidity.is_empty() {
                    "—".to_string()
                } else {
                    format!("{}%", weather.humidity)
                };
                content = content
                    .child(panel_section_title("CURRENT CONDITIONS"))
                    .child(panel_text_row("Feels like", &feels, false))
                    .child(panel_text_row("Wind", &wind, false))
                    .child(panel_text_row("Humidity", &humidity, false));
                content = content.child(panel_section_title("FORECAST"));
                if weather.forecast.is_empty() {
                    content = content.child(panel_empty_row("Forecast unavailable"));
                } else {
                    let mut forecast = div().flex().gap_2();
                    for day in &weather.forecast {
                        forecast = forecast.child(weather_day_card(day));
                    }
                    content = content.child(forecast);
                }
                Some(content)
            }
            "omarchy.audio" => {
                content = content.child(panel_hero(
                    "Audio",
                    self.system.audio.output_percent.map_or_else(
                        || "Output unavailable".to_string(),
                        |value| format!("{value}%"),
                    ),
                ));
                content = content
                    .child(panel_section_title("OUTPUT"))
                    .child(panel_meter(
                        self.system.audio.output_percent,
                        self.system.audio.output_muted,
                    ));
                for node in &self.system.audio.sinks {
                    content = content.child(panel_node_row(
                        &node.description,
                        node.volume,
                        node.is_default,
                        node.muted,
                    ));
                }
                if !self.system.audio.sources.is_empty() {
                    content = content.child(panel_section_title("INPUT"));
                    for node in &self.system.audio.sources {
                        content = content.child(panel_node_row(
                            &node.description,
                            node.volume,
                            node.is_default,
                            node.muted,
                        ));
                    }
                }
                if !self.system.audio.streams.is_empty() {
                    content = content.child(panel_section_title("STREAMS"));
                    for node in &self.system.audio.streams {
                        let label = if node.application.is_empty() {
                            &node.description
                        } else {
                            &node.application
                        };
                        content =
                            content.child(panel_node_row(label, node.volume, false, node.muted));
                    }
                }
                Some(content)
            }
            "omarchy.bluetooth" => {
                content = content.child(panel_hero(
                    "Bluetooth",
                    if self.system.bluetooth.powered {
                        format!("{} connected", self.system.bluetooth.connected_devices)
                    } else {
                        "Powered off".to_string()
                    },
                ));
                content = content.child(panel_section_title("DEVICES"));
                if self.system.bluetooth.devices.is_empty() {
                    content = content.child(panel_empty_row("No discovered devices"));
                } else {
                    for device in &self.system.bluetooth.devices {
                        content = content.child(panel_text_row(
                            &device.name,
                            if device.connected {
                                "Connected"
                            } else {
                                "Available"
                            },
                            device.connected,
                        ));
                    }
                }
                Some(content)
            }
            "omarchy.monitor" => {
                content = content.child(panel_hero(
                    "Monitor",
                    display_or_dash(&self.system.display.focused_monitor),
                ));
                content = content
                    .child(panel_section_title("BRIGHTNESS"))
                    .child(panel_meter(self.system.display.brightness_percent, false));
                content = content.child(panel_section_title("DISPLAYS"));
                for display in &self.system.display.displays {
                    let dimensions = if display.width > 0 && display.height > 0 {
                        format!("{}×{}", display.width, display.height)
                    } else {
                        "unknown size".to_string()
                    };
                    content = content.child(panel_text_row(
                        &display.name,
                        &format!(
                            "{} · {}",
                            dimensions,
                            if display.enabled {
                                "Enabled"
                            } else {
                                "Disabled"
                            }
                        ),
                        display.focused,
                    ));
                }
                content = content
                    .child(panel_section_title("SCALING"))
                    .child(panel_text_row(
                        "Scale",
                        &display_or_dash(&self.system.display.monitor_scale),
                        false,
                    ))
                    .child(panel_text_row(
                        "Text size",
                        &self
                            .system
                            .display
                            .text_size
                            .map_or_else(|| "—".to_string(), |value| format!("{value}px")),
                        false,
                    ));
                Some(content)
            }
            "omarchy.network" => {
                let connection = if self.system.network.connection.is_empty()
                    || self.system.network.connection == "--"
                {
                    "Disconnected".to_string()
                } else {
                    self.system.network.connection.clone()
                };
                content = content.child(panel_hero("Network", connection));
                content = content
                    .child(panel_section_title("SIGNAL"))
                    .child(panel_meter(
                        self.system.network.signal_percent,
                        self.system.network.wifi_enabled == Some(false),
                    ))
                    .child(panel_section_title("CONNECTION"));
                for (label, value) in [
                    (
                        "Interface",
                        display_or_dash(&self.system.network.details.iface),
                    ),
                    ("Address", display_or_dash(&self.system.network.details.ip)),
                    (
                        "Gateway",
                        display_or_dash(&self.system.network.details.gateway),
                    ),
                    (
                        "Router ping",
                        display_or_dash(&self.system.network.details.router_ping_ms),
                    ),
                    (
                        "Internet ping",
                        display_or_dash(&self.system.network.details.internet_ping_ms),
                    ),
                ] {
                    content = content.child(panel_text_row(label, &value, false));
                }
                if !self.system.network.wifi_networks.is_empty() {
                    content = content.child(panel_section_title("WI-FI NETWORKS"));
                    for network in &self.system.network.wifi_networks {
                        if network.ssid.is_empty() {
                            continue;
                        }
                        let detail = format!(
                            "{}% · {}{}",
                            network.signal_percent.max(0),
                            display_or_dash(&network.security),
                            if network.connected {
                                " · Connected"
                            } else {
                                ""
                            }
                        );
                        content = content.child(panel_text_row(
                            &network.ssid,
                            &detail,
                            network.connected,
                        ));
                    }
                }
                if !self.system.network.band.available.is_empty() {
                    content = content
                        .child(panel_section_title("WI-FI BAND"))
                        .child(panel_text_row(
                            "Selected",
                            &display_or_dash(&self.system.network.band.selected),
                            false,
                        ))
                        .child(panel_text_row(
                            "Current",
                            &display_or_dash(&self.system.network.band.current),
                            false,
                        ));
                }
                Some(content)
            }
            "omarchy.power" => {
                let battery = self.system.battery.percentage.map_or_else(
                    || "Battery unavailable".to_string(),
                    |value| format!("{value}%"),
                );
                content = content
                    .child(panel_hero("Power", battery))
                    .child(panel_meter(self.system.battery.percentage, false))
                    .child(panel_section_title("BATTERY"));
                for (label, value) in [
                    ("State", display_or_dash(&self.system.battery.state)),
                    ("Rate", display_or_dash(&self.system.battery.rate)),
                    ("Time", display_or_dash(&self.system.battery.time_remaining)),
                    ("Size", display_or_dash(&self.system.battery.size)),
                    ("Cycles", display_or_dash(&self.system.battery.cycles)),
                    ("Threshold", display_or_dash(&self.system.battery.threshold)),
                ] {
                    content = content.child(panel_text_row(label, &value, false));
                }
                if !self.system.power.profiles.is_empty() {
                    content = content.child(panel_section_title("POWER PROFILE"));
                    for profile in &self.system.power.profiles {
                        content = content.child(panel_text_row(
                            &profile.name,
                            if profile.active {
                                "Active"
                            } else {
                                "Available"
                            },
                            profile.active,
                        ));
                    }
                }
                Some(content)
            }
            "omarchy.media" => {
                let title = if self.system.media.title.is_empty() {
                    "No active track".to_string()
                } else {
                    self.system.media.title.clone()
                };
                let artist = if self.system.media.artist.is_empty() {
                    display_or_dash(&self.system.media.player)
                } else {
                    self.system.media.artist.clone()
                };
                content = content
                    .child(panel_hero("Media", title))
                    .child(panel_text_row("Artist", &artist, false))
                    .child(panel_text_row(
                        "Status",
                        &display_or_dash(&self.system.media.status),
                        self.system.media.status.eq_ignore_ascii_case("playing"),
                    ));
                if !self.system.media.players.is_empty() {
                    content = content.child(panel_section_title("PLAYERS"));
                    for player in &self.system.media.players {
                        let label = if player.player.is_empty() {
                            &player.desktop_entry
                        } else {
                            &player.player
                        };
                        let detail = if player.title.is_empty() {
                            display_or_dash(&player.status)
                        } else {
                            format!("{} · {}", player.status, truncate(&player.title, 28))
                        };
                        content = content.child(panel_text_row(
                            label,
                            &detail,
                            player.status.eq_ignore_ascii_case("playing"),
                        ));
                    }
                }
                Some(content)
            }
            _ => None,
        }
    }
}

impl Render for PanelView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .dmenu
            .as_ref()
            .map(|request| request.prompt.clone())
            .or_else(|| {
                self.menu
                    .as_ref()
                    .and_then(|menu| menu.item(&self.active_menu))
                    .map(|item| {
                        if item.title.is_empty() {
                            item.label.clone()
                        } else {
                            item.title.clone()
                        }
                    })
            })
            .unwrap_or_else(|| label_for(&self.id).to_string());
        let content = if self.dmenu.is_some() {
            self.dmenu_content(cx)
        } else if self.menu.is_some() {
            self.menu_content(cx)
        } else if self.id == "omarchy.wifiqr" {
            self.wifi_qr_content()
        } else if self.id == "omarchy.notifications" {
            self.notification_content()
        } else if self.id == "omarchy.osd" {
            self.osd_content()
        } else if self.is_overlay() {
            self.overlay_content(cx)
        } else if let Some(content) = self.rich_panel_content(cx) {
            content
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
            | "omarchy.notifications"
    )
}

fn is_bar_actionable(id: &str) -> bool {
    matches!(
        id,
        "omarchy.active-window"
            | "omarchy.keyboard-layout"
            | "omarchy.microphone"
            | "omarchy.system-update"
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

fn parse_osd_payload(payload: &str) -> (String, String) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return (String::new(), String::new());
    };
    (
        value
            .get("icon")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

fn parse_notification_history(payload: &str) -> Vec<NotificationEntry> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Vec::new();
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some(NotificationEntry {
                id: entry
                    .get("id")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|id| u32::try_from(id).ok())
                    .unwrap_or_default(),
                app: entry
                    .get("app")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                app_icon: entry
                    .get("appIcon")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                summary: entry
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                body: entry
                    .get("body")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                image: entry
                    .get("image")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                glyph: entry
                    .get("glyph")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                exec_argv: entry
                    .get("execArgv")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                urgency: entry
                    .get("urgency")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(1),
                expire_timeout: entry
                    .get("expireTimeout")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn notification_lifetime(entry: &NotificationEntry) -> Option<Duration> {
    if entry.urgency >= 2 && entry.expire_timeout == 0 {
        return None;
    }
    let milliseconds = if entry.expire_timeout > 0 {
        u64::from(entry.expire_timeout)
    } else if entry.urgency == 0 {
        8_000
    } else {
        5_000
    };
    Some(Duration::from_millis(milliseconds.clamp(1_000, 20_000)))
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
            (
                "Output devices".to_string(),
                audio_node_summary(&system.audio.sinks),
            ),
            (
                "Input devices".to_string(),
                audio_node_summary(&system.audio.sources),
            ),
            (
                "Playback streams".to_string(),
                audio_node_summary(&system.audio.streams),
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
        "omarchy.indicators" => vec![
            (
                "Night light".to_string(),
                if system.nightlight.active {
                    "active".to_string()
                } else {
                    "inactive".to_string()
                },
            ),
            ("DND".to_string(), "service-backed".to_string()),
            (
                "Stay awake".to_string(),
                if plugins.idle.stay_awake {
                    "active".to_string()
                } else {
                    "inactive".to_string()
                },
            ),
        ],
        "omarchy.battery" => vec![
            (
                "Percentage".to_string(),
                system
                    .battery
                    .percentage
                    .map(|value| format!("{value}%"))
                    .unwrap_or_else(|| "Unavailable".to_string()),
            ),
            ("State".to_string(), display_or_dash(&system.battery.state)),
            ("Charging".to_string(), yes_no(system.battery.charging)),
        ],
        "omarchy.idle" => vec![
            ("Enabled".to_string(), yes_no(plugins.idle.enabled)),
            ("Stay awake".to_string(), yes_no(plugins.idle.stay_awake)),
            (
                "State file".to_string(),
                display_or_dash(&plugins.idle.state_path.display().to_string()),
            ),
        ],
        "omarchy.nightlight" => vec![
            ("Enabled".to_string(), yes_no(system.nightlight.active)),
            (
                "Temperature".to_string(),
                system
                    .nightlight
                    .temperature
                    .map(|value| format!("{value}K"))
                    .unwrap_or_else(|| "Unavailable".to_string()),
            ),
        ],
        "omarchy.dev-gallery" => vec![
            ("Surface".to_string(), "GPUI component gallery".to_string()),
            ("Status".to_string(), "available".to_string()),
        ],
        "omarchy.disk-speedtest" => vec![
            ("Tool".to_string(), "omarchy-disk-speedtest".to_string()),
            ("Status".to_string(), "idle until requested".to_string()),
        ],
        "omarchy.speedtest" => vec![
            ("Tool".to_string(), "omarchy-network-speedtest".to_string()),
            ("Status".to_string(), "idle until requested".to_string()),
        ],
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
            (
                "Authenticated".to_string(),
                yes_no(plugins.dropbox.authenticated),
            ),
            (
                "Status".to_string(),
                display_or_dash(&plugins.dropbox.status_text),
            ),
            (
                "Usage".to_string(),
                if plugins.dropbox.quota_known {
                    format!(
                        "{}% ({}/{})",
                        plugins.dropbox.usage_percent.round() as u64,
                        plugins.dropbox.used_bytes,
                        plugins.dropbox.quota_bytes
                    )
                } else {
                    plugins.dropbox.used_bytes.to_string()
                },
            ),
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

fn panel_hero(title: &str, detail: String) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .p_3()
        .rounded_md()
        .bg(rgb(0x27272a))
        .border_1()
        .border_color(rgb(0x52525b))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_size(px(17.0)).child(title.to_string()))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(0xa1a1aa))
                        .child(detail),
                ),
        )
}

fn panel_section_title(title: &str) -> Div {
    div()
        .mt_1()
        .text_size(px(11.0))
        .text_color(rgb(0xa1a1aa))
        .child(title.to_string())
}

fn panel_meter(percent: Option<u8>, muted: bool) -> Div {
    let value = percent.unwrap_or_default().min(100);
    let fill_width = 7.2 * f32::from(value);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .justify_between()
                .text_size(px(11.0))
                .text_color(rgb(0xa1a1aa))
                .child(if muted {
                    "Muted".to_string()
                } else if percent.is_some() {
                    format!("{value}%")
                } else {
                    "Unavailable".to_string()
                }),
        )
        .child(
            div()
                .w_full()
                .h(px(7.0))
                .rounded_md()
                .bg(rgb(0x3f3f46))
                .child(
                    div()
                        .h(px(7.0))
                        .rounded_md()
                        .bg(if muted { rgb(0x71717a) } else { rgb(0xa78bfa) })
                        .w(px(fill_width)),
                ),
        )
}

fn panel_node_row(label: &str, volume: Option<u8>, is_default: bool, muted: bool) -> Div {
    let detail = format!(
        "{}{}{}",
        volume.map_or_else(|| "—".to_string(), |value| format!("{value}%")),
        if muted { " · Muted" } else { "" },
        if is_default { " · Default" } else { "" }
    );
    panel_text_row(label, &detail, is_default)
}

fn panel_text_row(label: &str, detail: &str, highlighted: bool) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(if highlighted {
            rgb(0x3f3f46)
        } else {
            rgb(0x27272a)
        })
        .child(div().text_color(rgb(0xf4f4f5)).child(truncate(label, 44)))
        .child(div().text_color(rgb(0xa1a1aa)).child(truncate(detail, 44)))
}

fn panel_empty_row(label: &str) -> Div {
    div()
        .px_3()
        .py_2()
        .text_color(rgb(0xa1a1aa))
        .child(label.to_string())
}

fn weather_day_card(day: &WeatherDay) -> Div {
    let maximum = if day.max_c.is_empty() {
        "—".to_string()
    } else {
        format!("{}°", day.max_c)
    };
    let minimum = if day.min_c.is_empty() {
        "—".to_string()
    } else {
        format!("{}°", day.min_c)
    };
    div()
        .w(px(112.0))
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(rgb(0x27272a))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(0xa1a1aa))
                .child(forecast_day_label(&day.date)),
        )
        .child(div().text_size(px(24.0)).child(if day.icon.is_empty() {
            "".to_string()
        } else {
            day.icon.clone()
        }))
        .child(
            div()
                .text_size(px(12.0))
                .child(format!("{maximum} / {minimum}")),
        )
}

fn forecast_day_label(date: &str) -> String {
    let mut parts = date.split('-');
    let year = parts.next().and_then(|value| value.parse::<i32>().ok());
    let month = parts.next().and_then(|value| value.parse::<i32>().ok());
    let day = parts.next().and_then(|value| value.parse::<i32>().ok());
    let Some((year, month, day)) = year
        .zip(month)
        .zip(day)
        .map(|((year, month), day)| (year, month, day))
    else {
        return date.to_string();
    };
    ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"]
        .get(weekday_sunday(year, month, day) as usize)
        .copied()
        .unwrap_or("DAY")
        .to_string()
}

fn display_or_dash(value: &str) -> String {
    if value.is_empty() || value == "--" {
        "—".to_string()
    } else {
        value.to_string()
    }
}

fn audio_node_summary(nodes: &[crate::system::AudioNode]) -> String {
    if nodes.is_empty() {
        return "—".to_string();
    }
    nodes
        .iter()
        .map(|node| {
            let name = if node.application.is_empty() {
                &node.description
            } else {
                &node.application
            };
            let volume = node
                .volume
                .map_or_else(|| "—".to_string(), |value| format!("{value}%"));
            format!(
                "{} {}{}",
                truncate(name, 18),
                volume,
                if node.is_default { " ★" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn yes_no(value: bool) -> String {
    if value {
        "Yes".to_string()
    } else {
        "No".to_string()
    }
}

fn indicator_bar_items(
    indicators: &IndicatorState,
    system: &SystemSnapshot,
) -> Vec<(String, String, bool)> {
    vec![
        (
            "Dictation".to_string(),
            if indicators.dictation == "recording" {
                "󰍬".to_string()
            } else {
                "󰔟".to_string()
            },
            matches!(indicators.dictation.as_str(), "recording" | "transcribing"),
        ),
        (
            "ScreenRecording".to_string(),
            "󰻂".to_string(),
            indicators.recording,
        ),
        (
            "Reminder".to_string(),
            "󰢌".to_string(),
            indicators.reminder_count > 0,
        ),
        (
            "NightLight".to_string(),
            "󰔎".to_string(),
            system.nightlight.active,
        ),
        ("Dnd".to_string(), "󰂛".to_string(), indicators.dnd),
        (
            "StayAwake".to_string(),
            "󰅶".to_string(),
            indicators.stay_awake,
        ),
    ]
}

fn entry_visible(entry: &BarEntry, plugins: &PluginSnapshot, system: &SystemSnapshot) -> bool {
    match entry.id.as_str() {
        "omarchy.tray" => !plugins.tray.items.is_empty(),
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

fn tray_item_owned_by_omarchy(item: &TrayItem, snapshot: &ShellSnapshot) -> bool {
    let owned_name = format!(
        "{} {} {}",
        item.id.to_lowercase(),
        item.title.to_lowercase(),
        item.tooltip_title.to_lowercase()
    );
    if owned_name.contains("localsend") {
        return true;
    }
    let dropbox_is_configured = snapshot
        .left
        .iter()
        .chain(snapshot.center.iter())
        .chain(snapshot.right.iter())
        .any(|entry| entry.id == "omarchy.dropbox");
    dropbox_is_configured && owned_name.contains("dropbox")
}

fn tray_item_label(item: &TrayItem) -> String {
    let icon = if item.icon_name.is_empty() {
        &item.overlay_icon_name
    } else {
        &item.icon_name
    };
    let icon = icon.split('?').next().unwrap_or_default();
    let icon = icon.rsplit('/').next().unwrap_or(icon);
    if icon.is_empty() {
        "●".to_string()
    } else {
        format!("◈ {}", truncate(icon, 12))
    }
}

fn sanitize_id(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "item".to_string()
    } else {
        sanitized
    }
}

fn label_for_entry(
    entry: &BarEntry,
    clock: &str,
    vertical: bool,
    system: &SystemSnapshot,
    plugins: &PluginSnapshot,
) -> String {
    match entry.id.as_str() {
        "omarchy.clock" => clock_label(&entry.settings, clock, vertical),
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
            if !plugins.weather.icon.is_empty() && !plugins.weather.temp_c.is_empty() {
                format!("{} {}°C", plugins.weather.icon, plugins.weather.temp_c)
            } else if plugins.weather.status.is_empty() {
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
    use std::time::Duration;

    use super::{
        NotificationEntry, format_clock_pattern, menu_label, menu_matches_filter,
        notification_lifetime, parse_notification_history, parse_osd_payload,
    };
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

    #[test]
    fn parses_osd_and_notification_payloads_without_inventing_rows() {
        assert_eq!(
            parse_osd_payload(r#"{"icon":"volume","message":"50%"}"#),
            ("volume".to_string(), "50%".to_string())
        );
        let notifications =
            parse_notification_history(r#"[{"app":"mail","summary":"New mail","body":"Hello"}]"#);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].app, "mail");
        assert!(parse_notification_history("not-json").is_empty());
    }

    #[test]
    fn notification_lifetime_preserves_critical_and_bounds_requested_timeouts() {
        let critical = NotificationEntry {
            urgency: 2,
            ..NotificationEntry::default()
        };
        assert!(notification_lifetime(&critical).is_none());
        let short = NotificationEntry {
            expire_timeout: 100,
            ..NotificationEntry::default()
        };
        assert_eq!(notification_lifetime(&short), Some(Duration::from_secs(1)));
        let long = NotificationEntry {
            expire_timeout: 60_000,
            ..NotificationEntry::default()
        };
        assert_eq!(notification_lifetime(&long), Some(Duration::from_secs(20)));
    }

    #[test]
    fn clock_format_matches_reference_tokens_and_iso_week() {
        let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
        local.tm_year = 126;
        local.tm_mon = 0;
        local.tm_mday = 1;
        local.tm_wday = 4;
        local.tm_yday = 0;
        local.tm_hour = 15;
        local.tm_min = 7;
        let formatted =
            format_clock_pattern("dddd HH:mm · h:mm AP · d MMMM 'W'ww yyyy · ''yy", &local);
        assert_eq!(
            formatted,
            "Thursday 15:07 · 3:07 PM · 1 January W01 2026 · '26"
        );
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
            return format_clock_pattern("HH:mm", &local);
        }
    }

    let minutes = (seconds / 60) % (24 * 60);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn local_date_parts() -> (i32, u8, u8) {
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
            return (
                local.tm_year + 1900,
                u8::try_from(local.tm_mon + 1).unwrap_or(1),
                u8::try_from(local.tm_mday).unwrap_or(1),
            );
        }
    }

    (1970, 1, 1)
}

fn calendar_month_name(month: u8) -> &'static str {
    [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ]
    .get(month.saturating_sub(1) as usize)
    .copied()
    .unwrap_or("January")
}

fn calendar_days_in_month(year: i32, month: u8) -> u8 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 31,
    }
}

fn calendar_weekday(year: i32, month: u8) -> u8 {
    match weekday_sunday(year, i32::from(month), 1) {
        0 => 7,
        weekday => u8::try_from(weekday).unwrap_or(1),
    }
}

fn clock_label(settings: &serde_json::Value, fallback: &str, vertical: bool) -> String {
    let key = if vertical { "verticalFormat" } else { "format" };
    let default = if vertical {
        "HH\n—\nmm"
    } else {
        "dddd HH:mm"
    };
    settings
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(local_clock_with_pattern)
        .unwrap_or_else(|| {
            if settings.get(key).is_some() {
                fallback.to_string()
            } else {
                local_clock_with_pattern(default)
            }
        })
}

fn local_clock_with_pattern(pattern: &str) -> String {
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
            return format_clock_pattern(pattern, &local);
        }
    }

    pattern.to_string()
}

fn format_clock_pattern(pattern: &str, local: &libc::tm) -> String {
    let weekday = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let weekday_short = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let month = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let month_short = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let weekday_index = usize::try_from(local.tm_wday).unwrap_or(0).min(6);
    let month_index = usize::try_from(local.tm_mon).unwrap_or(0).min(11);
    let hour24 = local.tm_hour.clamp(0, 23);
    let hour12 = match hour24 % 12 {
        0 => 12,
        value => value,
    };
    let day = local.tm_mday.clamp(1, 31);
    let year = local.tm_year + 1900;
    let iso_week = iso_week_number(local);
    let mut output = String::new();
    let chars = pattern.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\'' {
            if chars.get(index + 1) == Some(&'\'') {
                output.push('\'');
                index += 2;
                continue;
            }
            index += 1;
            while index < chars.len() && chars[index] != '\'' {
                output.push(chars[index]);
                index += 1;
            }
            if index < chars.len() {
                index += 1;
            }
            continue;
        }
        let remaining = &chars[index..];
        let (token, value) = if remaining.starts_with(&['d', 'd', 'd', 'd']) {
            ("dddd", weekday[weekday_index].to_string())
        } else if remaining.starts_with(&['d', 'd', 'd']) {
            ("ddd", weekday_short[weekday_index].to_string())
        } else if remaining.starts_with(&['M', 'M', 'M', 'M']) {
            ("MMMM", month[month_index].to_string())
        } else if remaining.starts_with(&['M', 'M', 'M']) {
            ("MMM", month_short[month_index].to_string())
        } else if remaining.starts_with(&['y', 'y', 'y', 'y']) {
            ("yyyy", format!("{year:04}"))
        } else if remaining.starts_with(&['y', 'y']) {
            ("yy", format!("{:02}", year.rem_euclid(100)))
        } else if remaining.starts_with(&['H', 'H']) {
            ("HH", format!("{hour24:02}"))
        } else if remaining.starts_with(&['m', 'm']) {
            ("mm", format!("{:02}", local.tm_min.clamp(0, 59)))
        } else if remaining.starts_with(&['s', 's']) {
            ("ss", format!("{:02}", local.tm_sec.clamp(0, 60)))
        } else if remaining.starts_with(&['A', 'P']) {
            (
                "AP",
                if hour24 < 12 {
                    "AM".to_string()
                } else {
                    "PM".to_string()
                },
            )
        } else if remaining.starts_with(&['w', 'w']) {
            ("ww", format!("{iso_week:02}"))
        } else if remaining.starts_with(&['d', 'd']) {
            ("dd", format!("{day:02}"))
        } else if remaining[0] == 'd' {
            ("d", day.to_string())
        } else if remaining[0] == 'h' {
            ("h", hour12.to_string())
        } else if remaining[0] == 'H' {
            ("H", hour24.to_string())
        } else if remaining[0] == 'M' {
            ("M", (local.tm_mon + 1).to_string())
        } else {
            output.push(remaining[0]);
            index += 1;
            continue;
        };
        output.push_str(&value);
        index += token.chars().count();
    }
    output
}

fn iso_week_number(local: &libc::tm) -> i32 {
    let weekday = if local.tm_wday == 0 { 7 } else { local.tm_wday };
    let ordinal = local.tm_yday + 1;
    let mut week = (ordinal - weekday + 10) / 7;
    if week < 1 {
        week = iso_weeks_in_year(local.tm_year + 1899);
    } else if week > iso_weeks_in_year(local.tm_year + 1900) {
        week = 1;
    }
    week
}

fn iso_weeks_in_year(year: i32) -> i32 {
    let jan1 = weekday_sunday(year, 1, 1);
    if jan1 == 4 || (jan1 == 3 && is_leap_year(year)) {
        53
    } else {
        52
    }
}

fn weekday_sunday(year: i32, month: i32, day: i32) -> i32 {
    let (year, month) = if month <= 2 {
        (year - 1, month + 12)
    } else {
        (year, month)
    };
    let century_year = year.rem_euclid(100);
    let century = year.div_euclid(100);
    let zeller = (day
        + (13 * (month + 1)) / 5
        + century_year
        + century_year / 4
        + century / 4
        + 5 * century)
        % 7;
    (zeller + 6) % 7
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
