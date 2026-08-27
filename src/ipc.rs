use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    net::Shutdown,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::config::ShellSnapshot;
use crate::dbus;
use crate::plugins::{parse_dropbox_status, parse_tailscale_status};
use crate::system::{SystemAction, SystemSnapshot, run_action};

pub const IPC_METHODS: &[&str] = &[
    "ping",
    "applyTheme",
    "summon",
    "hide",
    "toggle",
    "call",
    "rescanPlugins",
    "reloadConfig",
    "toggleBarTransparency",
    "setPluginEnabled",
    "enablePlugin",
    "putBarWidget",
    "moveBarWidget",
    "setBarWidget",
    "listPlugins",
    "listShellConfig",
    "debugBarGeometry",
    "togglePanelAt",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellCommand {
    Ping,
    ApplyTheme {
        colors: String,
        shell: String,
    },
    ListPlugins,
    ReloadConfig,
    RescanPlugins,
    ToggleBarTransparency,
    EnablePlugin {
        id: String,
        placement: String,
    },
    PutBarWidget {
        id: String,
        placement: String,
    },
    MoveBarWidget {
        id: String,
        placement: String,
    },
    SetBarWidget {
        id: String,
        key: String,
        value: String,
        selector: String,
    },
    ListShellConfig,
    DebugBarGeometry,
    TogglePanelAt {
        section: String,
        index: String,
    },
    Call {
        id: String,
        method: String,
        args: Vec<String>,
    },
    Summon {
        id: String,
        payload: String,
    },
    Hide {
        id: String,
    },
    Toggle {
        id: String,
        payload: String,
    },
    SetPluginEnabled {
        id: String,
        enabled: bool,
    },
    ImageSelectorOpen {
        payload: String,
    },
    ImageSelectorPreload {
        payload: String,
    },
    ImageSelectorCancel {
        done_file: String,
    },
    ImageSelectorPing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IpcEvent {
    Refresh,
    Background { path: String, instant: bool },
    Summon { id: String, payload: String },
    Hide { id: String },
    Toggle { id: String, payload: String },
    Lock { preview: bool },
    Notification { entry: String },
    NotificationHistory { entries: String },
}

pub type IpcEventReceiver = Arc<Mutex<Receiver<IpcEvent>>>;

struct IpcRuntime {
    snapshot: ShellSnapshot,
    open_panel_ids: BTreeSet<String>,
    pending_payloads: BTreeMap<String, Vec<String>>,
    dnd_enabled: bool,
    dnd_state_path: PathBuf,
    osd_open: bool,
    background_path: String,
    preferred_media_player: String,
    lock_requested: bool,
    lock_preview_visible: bool,
    lock_last_event: String,
    lock_last_event_at: String,
}

impl IpcRuntime {
    fn new(snapshot: ShellSnapshot) -> Self {
        let home = home_from_config_path(&snapshot.user_config_path);
        let dnd_state_path = home.join(".local/state/omarchy/notifications.json");
        Self {
            snapshot,
            open_panel_ids: BTreeSet::new(),
            pending_payloads: BTreeMap::new(),
            dnd_enabled: load_dnd_state(&dnd_state_path),
            dnd_state_path,
            osd_open: false,
            background_path: current_background_path(&home),
            preferred_media_player: String::new(),
            lock_requested: false,
            lock_preview_visible: false,
            lock_last_event: "init".to_string(),
            lock_last_event_at: String::new(),
        }
    }
}

struct CommandOutcome {
    output: String,
    event: Option<IpcEvent>,
}

/// A small JSON-lines Unix-socket bridge for the long-lived GPUI shell.
///
/// Omarchy's installed helper talks to a running Quickshell process.  Keeping
/// the transport separate from command parsing lets the same dispatch logic be
/// tested directly and used by the live GPUI process.  The wire envelope is
/// intentionally private to this port; `try_call_running` exposes the same
/// one-line result the CLI caller expects.
pub struct IpcServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    event_tx: Sender<IpcEvent>,
    thread: Option<JoinHandle<()>>,
}

impl IpcServer {
    pub fn start(snapshot: ShellSnapshot) -> Result<(Self, IpcEventReceiver), String> {
        let path = socket_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("create IPC directory: {error}"))?;
        }

        if path.exists() {
            match std::os::unix::net::UnixStream::connect(&path) {
                Ok(_) => {
                    return Err(format!(
                        "GPUI shell is already running at {}",
                        path.display()
                    ));
                }
                Err(_) => fs::remove_file(&path)
                    .map_err(|error| format!("remove stale IPC socket: {error}"))?,
            }
        }

        let listener = std::os::unix::net::UnixListener::bind(&path)
            .map_err(|error| format!("bind IPC socket {}: {error}", path.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("configure IPC socket: {error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let state = Arc::new(Mutex::new(IpcRuntime::new(snapshot)));
        let (event_tx, event_rx) = mpsc::channel();
        let thread_events = event_tx.clone();
        let thread = thread::Builder::new()
            .name("omarchy-gpui-ipc".to_string())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => handle_connection(stream, &state, &thread_events),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| format!("start IPC thread: {error}"))?;

        Ok((
            Self {
                path,
                stop,
                event_tx,
                thread: Some(thread),
            },
            Arc::new(Mutex::new(event_rx)),
        ))
    }

    pub fn event_sender(&self) -> Sender<IpcEvent> {
        self.event_tx.clone()
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::os::unix::net::UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

/// Try the live shell first. `None` means there is no active GPUI server and
/// the caller may use the direct command path for offline contract tests.
pub fn try_call_running(args: &[String]) -> Result<Option<String>, String> {
    let path = socket_path();
    let mut stream = match std::os::unix::net::UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(format!("connect to GPUI shell: {error}")),
    };
    let request = serde_json::json!({"args": args});
    stream
        .write_all(request.to_string().as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|error| format!("write GPUI shell IPC request: {error}"))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| format!("finish GPUI shell IPC request: {error}"))?;
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|error| format!("read GPUI shell IPC response: {error}"))?;
    let response: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|error| format!("invalid GPUI shell response: {error}"))?;
    if response.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(Some(
            response
                .get("output")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ))
    } else {
        Err(response
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("GPUI shell IPC request failed")
            .to_string())
    }
}

fn handle_connection(
    mut stream: std::os::unix::net::UnixStream,
    state: &Arc<Mutex<IpcRuntime>>,
    events: &Sender<IpcEvent>,
) {
    let mut raw = String::new();
    let (response, event) = match stream.read_to_string(&mut raw) {
        Ok(_) => handle_request(raw.trim(), state),
        Err(error) => (
            serde_json::json!({"ok": false, "error": format!("read request: {error}")}),
            None,
        ),
    };
    if let Some(event) = event {
        let _ = events.send(event);
    }
    let _ = stream.write_all(response.to_string().as_bytes());
    let _ = stream.write_all(b"\n");
}

fn handle_request(
    raw: &str,
    state: &Arc<Mutex<IpcRuntime>>,
) -> (serde_json::Value, Option<IpcEvent>) {
    let args = match serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.get("args").cloned())
        .and_then(|value| value.as_array().cloned())
        .and_then(|values| {
            values
                .into_iter()
                .map(|value| value.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        }) {
        Some(args) => args,
        None => {
            return (
                serde_json::json!({"ok": false, "error": "invalid IPC request"}),
                None,
            );
        }
    };

    let result = state
        .lock()
        .map_err(|_| "IPC state lock poisoned".to_string())
        .and_then(|mut runtime| {
            let command = parse(&args)?;
            dispatch_runtime(&command, &mut runtime)
        });
    match result {
        Ok(outcome) => (
            serde_json::json!({"ok": true, "output": outcome.output}),
            outcome.event,
        ),
        Err(error) => (serde_json::json!({"ok": false, "error": error}), None),
    }
}

fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("OMARCHY_GPUI_SOCKET") {
        return PathBuf::from(path);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    runtime.join("omarchy-gpui-shell.sock")
}

pub fn parse(args: &[String]) -> Result<ShellCommand, String> {
    if args.first().map(String::as_str) != Some("shell") {
        return parse_direct_target(args);
    }

    match args.get(1).map(String::as_str) {
        Some("ping") => Ok(ShellCommand::Ping),
        Some("applyTheme") => Ok(ShellCommand::ApplyTheme {
            colors: required(args, 2, "colors payload")?,
            shell: required(args, 3, "shell payload")?,
        }),
        Some("listPlugins") => Ok(ShellCommand::ListPlugins),
        Some("reloadConfig") => Ok(ShellCommand::ReloadConfig),
        Some("rescanPlugins") => Ok(ShellCommand::RescanPlugins),
        Some("toggleBarTransparency") => Ok(ShellCommand::ToggleBarTransparency),
        Some("enablePlugin") => Ok(ShellCommand::EnablePlugin {
            id: required(args, 2, "plugin id")?,
            placement: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("putBarWidget") => Ok(ShellCommand::PutBarWidget {
            id: required(args, 2, "plugin id")?,
            placement: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("moveBarWidget") => Ok(ShellCommand::MoveBarWidget {
            id: required(args, 2, "plugin id")?,
            placement: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("setBarWidget") => Ok(ShellCommand::SetBarWidget {
            id: required(args, 2, "plugin id")?,
            key: required(args, 3, "widget setting")?,
            value: required(args, 4, "widget value")?,
            selector: args.get(5).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("listShellConfig") => Ok(ShellCommand::ListShellConfig),
        Some("debugBarGeometry") => Ok(ShellCommand::DebugBarGeometry),
        Some("togglePanelAt") => Ok(ShellCommand::TogglePanelAt {
            section: required(args, 2, "bar section")?,
            index: required(args, 3, "bar index")?,
        }),
        Some("call") => Ok(ShellCommand::Call {
            id: required(args, 2, "plugin id")?,
            method: required(args, 3, "plugin method")?,
            args: args.get(4..).unwrap_or_default().to_vec(),
        }),
        Some("summon") => Ok(ShellCommand::Summon {
            id: required(args, 2, "plugin id")?,
            payload: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("hide") => Ok(ShellCommand::Hide {
            id: required(args, 2, "plugin id")?,
        }),
        Some("toggle") => Ok(ShellCommand::Toggle {
            id: required(args, 2, "plugin id")?,
            payload: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("setPluginEnabled") => Ok(ShellCommand::SetPluginEnabled {
            id: required(args, 2, "plugin id")?,
            enabled: args.get(3).is_some_and(|value| value == "true"),
        }),
        Some(method) => Err(format!("unknown shell method: {method}")),
        None => Err("missing shell method".to_string()),
    }
}

fn parse_direct_target(args: &[String]) -> Result<ShellCommand, String> {
    let target = required(args, 0, "IPC target")?;
    let method = required(args, 1, "IPC method")?;

    if target == "image-selector" {
        return match method.as_str() {
            "open" => {
                let image_dirs = positional(args, 2, "image directories")?;
                let image_rows = decode_utf8(positional(args, 3, "image rows")?);
                let selected_image = positional(args, 4, "selected image")?;
                let selection_file = positional(args, 5, "selection file")?;
                let done_file = positional(args, 6, "done file")?;
                let show_labels = positional(args, 7, "show labels")?;
                let filterable = positional(args, 8, "filterable")?;
                Ok(ShellCommand::ImageSelectorOpen {
                    payload: serde_json::json!({
                        "imageDirs": image_dirs,
                        "imageRows": image_rows,
                        "selectedImage": selected_image,
                        "selectionFile": selection_file,
                        "doneFile": done_file,
                        "showLabels": show_labels,
                        "filterable": filterable,
                    })
                    .to_string(),
                })
            }
            "preload" => {
                let image_rows = decode_utf8(positional(args, 2, "image rows")?);
                let selected_image = positional(args, 3, "selected image")?;
                let show_labels = positional(args, 4, "show labels")?;
                let filterable = positional(args, 5, "filterable")?;
                Ok(ShellCommand::ImageSelectorPreload {
                    payload: serde_json::json!({
                        "imageRows": image_rows,
                        "selectedImage": selected_image,
                        "showLabels": show_labels,
                        "filterable": filterable,
                    })
                    .to_string(),
                })
            }
            "cancel" => Ok(ShellCommand::ImageSelectorCancel {
                done_file: args.get(2).cloned().unwrap_or_default(),
            }),
            "ping" => Ok(ShellCommand::ImageSelectorPing),
            _ => Err(format!("unknown image-selector method: {method}")),
        };
    }

    // The installed helper invokes service targets directly (`lock lock`,
    // `nightlight toggle`, and similar). Those targets all currently accept a
    // single string argument; root `shell call` remains the canonical path for
    // that same one-argument contract.
    Ok(ShellCommand::Call {
        id: target,
        method,
        args: args.get(2..).unwrap_or_default().to_vec(),
    })
}

pub fn dispatch(command: &ShellCommand, snapshot: &mut ShellSnapshot) -> Result<String, String> {
    let mut runtime = IpcRuntime::new(snapshot.clone());
    let outcome = dispatch_runtime(command, &mut runtime)?;
    *snapshot = runtime.snapshot;
    Ok(outcome.output)
}

fn dispatch_runtime(
    command: &ShellCommand,
    runtime: &mut IpcRuntime,
) -> Result<CommandOutcome, String> {
    let snapshot = &mut runtime.snapshot;
    match command {
        ShellCommand::Ping => Ok(outcome("ok", None)),
        ShellCommand::ApplyTheme { colors, shell } => {
            // GPUI theme application is process-local for now. Decode both
            // payloads here so malformed base64 follows the same fail-open
            // contract as the reference shell while the theme adapter is
            // wired into the renderer.
            let _ = decode_base64(colors);
            let _ = decode_base64(shell);
            Ok(outcome("ok", None))
        }
        ShellCommand::ReloadConfig => {
            snapshot.reload();
            Ok(outcome("ok", Some(IpcEvent::Refresh)))
        }
        ShellCommand::RescanPlugins => {
            snapshot.reload();
            Ok(outcome("", Some(IpcEvent::Refresh)))
        }
        ShellCommand::ToggleBarTransparency => {
            snapshot.toggle_bar_transparency()?;
            Ok(outcome("ok", Some(IpcEvent::Refresh)))
        }
        ShellCommand::EnablePlugin { id, placement } => {
            let placement = parse_json_object(placement, "placement")?;
            if snapshot.set_plugin_enabled(id, true, Some(&placement))? {
                Ok(outcome("ok", Some(IpcEvent::Refresh)))
            } else {
                Ok(outcome("unknown", None))
            }
        }
        ShellCommand::PutBarWidget { id, placement } => {
            let placement = parse_json_object(placement, "placement")?;
            let error = snapshot.put_bar_widget(id, Some(&placement))?;
            if error.is_empty() {
                Ok(outcome("ok", Some(IpcEvent::Refresh)))
            } else {
                Ok(outcome(&error, None))
            }
        }
        ShellCommand::MoveBarWidget { id, placement } => {
            let placement = parse_json_object(placement, "placement")?;
            let error = snapshot.move_bar_widget(id, &placement)?;
            if error.is_empty() {
                Ok(outcome("ok", Some(IpcEvent::Refresh)))
            } else {
                Ok(outcome(&error, None))
            }
        }
        ShellCommand::SetBarWidget {
            id,
            key,
            value,
            selector,
        } => {
            let value = serde_json::from_str(value)
                .map_err(|error| format!("invalid widget setting: {error}"))?;
            let selector = parse_json_object(selector, "selector")?;
            let error = snapshot.set_bar_widget(id, key, value, Some(&selector))?;
            if error.is_empty() {
                Ok(outcome("ok", Some(IpcEvent::Refresh)))
            } else {
                Ok(outcome(&error, None))
            }
        }
        ShellCommand::ListPlugins => Ok(outcome(&plugin_list(snapshot), None)),
        ShellCommand::ListShellConfig => Ok(outcome(&snapshot.config.to_string(), None)),
        ShellCommand::DebugBarGeometry => Ok(outcome("[]", None)),
        ShellCommand::TogglePanelAt { section, index } => {
            let Some(id) = panel_widget_id_at(snapshot, section, index) else {
                return Ok(outcome("unknown", None));
            };
            let event = toggle_panel(runtime, &id, "{}").map(|_| IpcEvent::Toggle {
                id: id.clone(),
                payload: "{}".to_string(),
            });
            Ok(outcome(&id, event))
        }
        ShellCommand::Call { id, method, args } => dispatch_call(runtime, id, method, args),
        ShellCommand::Summon { id, payload } => {
            let Some(resolved) = summon_panel(runtime, id, payload) else {
                return Ok(outcome("unknown", None));
            };
            Ok(outcome(
                "ok",
                Some(IpcEvent::Summon {
                    id: resolved,
                    payload: payload.clone(),
                }),
            ))
        }
        ShellCommand::Hide { id } => {
            let resolved = resolve_enabled_id(snapshot, id);
            let removed = runtime.open_panel_ids.remove(&resolved);
            if removed || snapshot.plugin(&resolved).is_some() {
                Ok(outcome("", Some(IpcEvent::Hide { id: resolved })))
            } else {
                Ok(outcome("", None))
            }
        }
        ShellCommand::Toggle { id, payload } => {
            let resolved = resolve_enabled_id(snapshot, id);
            if runtime.open_panel_ids.remove(&resolved) {
                Ok(outcome("", Some(IpcEvent::Hide { id: resolved })))
            } else {
                let Some(resolved) = summon_panel(runtime, &resolved, payload) else {
                    return Ok(outcome("", None));
                };
                Ok(outcome(
                    "",
                    Some(IpcEvent::Toggle {
                        id: resolved,
                        payload: payload.clone(),
                    }),
                ))
            }
        }
        ShellCommand::SetPluginEnabled { id, enabled } => {
            if snapshot.set_plugin_enabled(id, *enabled, None)? {
                Ok(outcome("ok", Some(IpcEvent::Refresh)))
            } else {
                Ok(outcome("unknown", None))
            }
        }
        ShellCommand::ImageSelectorOpen { payload } => {
            let Some(resolved) = summon_panel(runtime, "omarchy.image-picker", payload) else {
                return Ok(outcome("unknown", None));
            };
            Ok(outcome(
                "ok",
                Some(IpcEvent::Summon {
                    id: resolved,
                    payload: payload.clone(),
                }),
            ))
        }
        ShellCommand::ImageSelectorPreload { payload } => {
            runtime
                .pending_payloads
                .entry("omarchy.image-picker".to_string())
                .or_default()
                .push(payload.clone());
            Ok(outcome("ok", None))
        }
        ShellCommand::ImageSelectorCancel { done_file } => {
            finish_done_file(done_file)?;
            runtime.open_panel_ids.remove("omarchy.image-picker");
            Ok(outcome(
                "ok",
                Some(IpcEvent::Hide {
                    id: "omarchy.image-picker".to_string(),
                }),
            ))
        }
        ShellCommand::ImageSelectorPing => Ok(outcome("ok", None)),
    }
}

fn dispatch_call(
    runtime: &mut IpcRuntime,
    requested_id: &str,
    method: &str,
    args: &[String],
) -> Result<CommandOutcome, String> {
    let arg = args.first().map(String::as_str).unwrap_or_default();
    let Some(id) = resolve_call_target(&runtime.snapshot, requested_id) else {
        return Ok(outcome("unknown", None));
    };
    if !runtime.snapshot.plugin_is_enabled(&id)
        || !call_target_is_loaded(runtime, &id)
        || !call_method_supported(&id, method)
    {
        return Ok(outcome("unknown", None));
    }

    if matches!(method, "open" | "show" | "close" | "hide" | "toggle")
        && !(id == "omarchy.osd" && method == "show")
    {
        return Ok(dispatch_call_lifecycle(runtime, &id, method, arg));
    }

    let result = match (id.as_str(), method) {
        ("omarchy.agents", "refresh" | "next")
        | ("omarchy.indicators", "refresh")
        | ("omarchy.system-update", "refresh" | "clear")
        | ("omarchy.menu", "refresh")
        | ("omarchy.clock", "refresh" | "cycleFormat" | "toggleWeekStart")
        | ("omarchy.dropbox", "refresh")
        | ("omarchy.tailscale", "refresh")
        | ("omarchy.weather", "refresh")
        | ("omarchy.nightlight", "refresh") => Ok(outcome("ok", Some(IpcEvent::Refresh))),

        ("omarchy.dropbox", "login") => {
            if command_present("dropbox-cli") {
                let _ = Command::new("dropbox-cli").arg("start").spawn();
            }
            Ok(outcome("ok", None))
        }

        ("omarchy.background", "refresh") => {
            let home = home_from_config_path(&runtime.snapshot.user_config_path);
            runtime.background_path = current_background_path(&home);
            Ok(outcome(
                "",
                Some(IpcEvent::Background {
                    path: runtime.background_path.clone(),
                    instant: false,
                }),
            ))
        }
        ("omarchy.background", "set" | "setInstant") => {
            let path = required_call_arg(args, 0, "background path")?;
            if path.trim().is_empty() {
                Ok(outcome("", None))
            } else {
                runtime.background_path = path.to_string();
                Ok(outcome(
                    "",
                    Some(IpcEvent::Background {
                        path: runtime.background_path.clone(),
                        instant: method == "setInstant",
                    }),
                ))
            }
        }
        ("omarchy.background", "transition") => {
            let path = required_call_arg(args, 1, "background path")?;
            if path.trim().is_empty() {
                Ok(outcome("", None))
            } else {
                runtime.background_path = path.to_string();
                Ok(outcome(
                    "",
                    Some(IpcEvent::Background {
                        path: runtime.background_path.clone(),
                        instant: false,
                    }),
                ))
            }
        }
        ("omarchy.background", "themeTransition") => {
            let path = required_call_arg(args, 1, "background path")?;
            let _final_path = required_call_arg(args, 2, "final background path")?;
            let _colors = required_call_arg(args, 3, "colors payload")?;
            let _shell = required_call_arg(args, 4, "shell payload")?;
            if path.trim().is_empty() {
                Ok(outcome("", None))
            } else {
                runtime.background_path = path.to_string();
                Ok(outcome(
                    "",
                    Some(IpcEvent::Background {
                        path: runtime.background_path.clone(),
                        instant: false,
                    }),
                ))
            }
        }

        ("omarchy.notifications", "dndState" | "isDnd") => Ok(outcome(
            if runtime.dnd_enabled { "on" } else { "off" },
            None,
        )),
        ("omarchy.notifications", "toggleDnd") => {
            runtime.dnd_enabled = !runtime.dnd_enabled;
            let _ = persist_dnd_state(&runtime.dnd_state_path, runtime.dnd_enabled);
            Ok(outcome(
                if runtime.dnd_enabled { "on" } else { "off" },
                None,
            ))
        }
        ("omarchy.notifications", "setDnd") => {
            runtime.dnd_enabled = parse_bool_arg(required_call_arg(args, 0, "DND value")?);
            let _ = persist_dnd_state(&runtime.dnd_state_path, runtime.dnd_enabled);
            Ok(outcome(
                if runtime.dnd_enabled { "on" } else { "off" },
                None,
            ))
        }
        ("omarchy.notifications", "clear") => {
            clear_notification_history(&home_from_config_path(&runtime.snapshot.user_config_path))?;
            Ok(outcome("ok", None))
        }
        ("omarchy.notifications", "dismissAll") => {
            clear_live_notifications(&home_from_config_path(&runtime.snapshot.user_config_path))?;
            Ok(outcome("ok", None))
        }
        ("omarchy.notifications", "dismissOne") => {
            let home = home_from_config_path(&runtime.snapshot.user_config_path);
            let output = if remove_latest_live_notification(&home)? {
                "ok"
            } else {
                "none"
            };
            Ok(outcome(output, None))
        }
        ("omarchy.notifications", "invokeLast") => {
            let home = home_from_config_path(&runtime.snapshot.user_config_path);
            let output = if invoke_latest_live_notification(&home)? {
                "ok"
            } else {
                "none"
            };
            Ok(outcome(output, None))
        }
        ("omarchy.notifications", "dismiss") => {
            let summary = required_call_arg(args, 0, "notification summary")?;
            let removed = dismiss_notifications_matching(
                &home_from_config_path(&runtime.snapshot.user_config_path),
                summary,
            )?;
            Ok(outcome(if removed { "ok" } else { "none" }, None))
        }
        ("omarchy.notifications", "showHistory") => {
            let entries = read_notification_history(&home_from_config_path(
                &runtime.snapshot.user_config_path,
            ))?;
            Ok(outcome(
                "ok",
                Some(IpcEvent::NotificationHistory { entries }),
            ))
        }
        ("omarchy.notifications", "ping") => Ok(outcome("ok", None)),
        ("omarchy.menu", "ping") => Ok(outcome("ok", None)),

        ("omarchy.osd", "show") => {
            runtime.osd_open = true;
            Ok(outcome(
                "ok",
                Some(IpcEvent::Summon {
                    id: id.clone(),
                    payload: arg.to_string(),
                }),
            ))
        }
        ("omarchy.osd", "close") => {
            runtime.osd_open = false;
            runtime.open_panel_ids.remove(&id);
            Ok(outcome("ok", Some(IpcEvent::Hide { id: id.clone() })))
        }
        ("omarchy.osd", "state") => Ok(outcome(
            if runtime.osd_open { "open" } else { "closed" },
            None,
        )),
        ("omarchy.osd", "ping") => Ok(outcome("ok", None)),

        ("omarchy.monitor", "brightness") => {
            let percent = parse_brightness_arg(arg);
            let monitor = SystemSnapshot::collect().display.focused_monitor;
            if monitor.is_empty() {
                Ok(outcome(&format!("got {percent}"), None))
            } else {
                match run_action(&SystemAction::SetBrightness { monitor, percent }) {
                    Ok(()) => Ok(outcome(&format!("got {percent}"), None)),
                    Err(_) => Ok(outcome("error", None)),
                }
            }
        }
        ("omarchy.monitor", "state") => {
            let display = SystemSnapshot::collect().display;
            let value = serde_json::json!({
                "brightness": display.brightness_percent.unwrap_or_default(),
                "brightnessAvailable": display.brightness_available,
                "focusedMonitor": display.focused_monitor,
                "scale": display.monitor_scale,
                "displays": display.displays.iter().map(|item| serde_json::json!({
                    "name": item.name,
                    "enabled": item.enabled,
                    "focused": item.focused,
                    "width": item.width,
                    "height": item.height,
                })).collect::<Vec<_>>(),
            });
            Ok(outcome(&value.to_string(), None))
        }

        ("omarchy.bluetooth", "toggleBluetooth") => {
            let powered = SystemSnapshot::collect().bluetooth.powered;
            match run_action(&SystemAction::SetBluetoothPower(!powered)) {
                Ok(()) => Ok(outcome("ok", None)),
                Err(_) => Ok(outcome("error", None)),
            }
        }
        ("omarchy.network", "toggleNetwork") => {
            let snapshot = SystemSnapshot::collect();
            let Some(enabled) = snapshot.network.wifi_enabled else {
                return Ok(outcome("error", None));
            };
            match run_action(&SystemAction::SetWifiEnabled(!enabled)) {
                Ok(()) => Ok(outcome("ok", None)),
                Err(_) => Ok(outcome("error", None)),
            }
        }
        ("omarchy.network", "showQr") => Ok(outcome(
            "ok",
            Some(IpcEvent::Summon {
                id: "omarchy.wifiqr".to_string(),
                payload: "{}".to_string(),
            }),
        )),
        ("omarchy.network", "speedTest") => Ok(outcome(
            "ok",
            Some(IpcEvent::Summon {
                id: "omarchy.speedtest".to_string(),
                payload: "{}".to_string(),
            }),
        )),

        ("omarchy.power", "togglePercentage") => {
            let current = snapshot_widget_bool(&runtime.snapshot, &id, "showPercentage");
            let error = runtime.snapshot.set_bar_widget(
                &id,
                "showPercentage",
                serde_json::Value::Bool(!current),
                None,
            )?;
            if error.is_empty() {
                Ok(outcome("ok", Some(IpcEvent::Refresh)))
            } else {
                Ok(outcome(&error, None))
            }
        }

        ("omarchy.media", "status") => {
            let media = SystemSnapshot::collect().media;
            let selected = if runtime.preferred_media_player.is_empty() {
                media
                    .players
                    .iter()
                    .find(|player| player.player == media.player)
                    .or_else(|| media.players.first())
            } else {
                media
                    .players
                    .iter()
                    .find(|player| player.key() == runtime.preferred_media_player)
                    .or_else(|| media.players.first())
            };
            let player = selected
                .map(|player| player.player.as_str())
                .unwrap_or_default();
            let status = selected
                .map(|player| player.status.as_str())
                .unwrap_or_default();
            let artist = selected
                .map(|player| player.artist.as_str())
                .unwrap_or_default();
            let title = selected
                .map(|player| player.title.as_str())
                .unwrap_or_default();
            let album = selected
                .map(|player| player.album.as_str())
                .unwrap_or_default();
            let art_url = selected
                .map(|player| player.art_url.as_str())
                .unwrap_or_default();
            let desktop_entry = selected
                .map(|player| player.desktop_entry.as_str())
                .unwrap_or_default();
            let value = serde_json::json!({
                "hasPlayer": selected.is_some(),
                "hasMedia": !title.is_empty() || !artist.is_empty(),
                "playing": status.eq_ignore_ascii_case("playing"),
                "identity": player,
                "desktopEntry": desktop_entry,
                "title": title,
                "artist": artist,
                "album": album,
                "artUrl": art_url,
                "canGoNext": selected.is_some_and(|player| player.can_go_next),
                "canGoPrevious": selected.is_some_and(|player| player.can_go_previous),
                "canTogglePlaying": selected.is_some_and(|player| player.can_toggle_playing()),
            });
            Ok(outcome(&value.to_string(), None))
        }
        ("omarchy.media", "playPause") => media_action(SystemAction::MediaPlayPause),
        ("omarchy.media", "next") => media_action(SystemAction::MediaNext),
        ("omarchy.media", "previous") => media_action(SystemAction::MediaPrevious),
        ("omarchy.media", "play") => media_action(SystemAction::MediaPlay),
        ("omarchy.media", "pause") => media_action(SystemAction::MediaPause),
        (
            "omarchy.media",
            "sourceNext" | "sourcePrevious" | "sourceSwitch" | "sourceSwitchPrevious",
        ) => {
            let media = SystemSnapshot::collect().media;
            let current = if runtime.preferred_media_player.is_empty() {
                media
                    .players
                    .iter()
                    .find(|player| player.player == media.player)
                    .map(|player| player.key())
                    .unwrap_or_default()
            } else {
                runtime.preferred_media_player.as_str()
            };
            let delta = if matches!(method, "sourcePrevious" | "sourceSwitchPrevious") {
                -1
            } else {
                1
            };
            let mut candidates = media
                .players
                .iter()
                .filter(|player| {
                    !player.key().is_empty() && player.has_metadata() && player.can_toggle_playing()
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| left.player.cmp(&right.player));
            if candidates.is_empty() {
                Ok(outcome("unhandled", None))
            } else {
                let current_index = candidates
                    .iter()
                    .position(|player| player.key() == current)
                    .unwrap_or(0);
                let next_index =
                    (current_index as isize + delta).rem_euclid(candidates.len() as isize) as usize;
                let next = candidates[next_index];
                let transfer = matches!(method, "sourceSwitch" | "sourceSwitchPrevious");
                if transfer
                    && next.key() != current
                    && media
                        .players
                        .iter()
                        .find(|player| player.key() == current)
                        .is_some_and(|player| player.status.eq_ignore_ascii_case("playing"))
                {
                    if media_player_action(next.key(), "play").is_err() {
                        return Ok(outcome("unhandled", None));
                    }
                    let _ = media_player_action(current, "pause");
                }
                runtime.preferred_media_player = next.key().to_string();
                Ok(outcome("ok", None))
            }
        }
        ("omarchy.media", "ping") => Ok(outcome("ok", None)),

        ("omarchy.nightlight", "status") => Ok(outcome(&nightlight_status_json(), None)),
        ("omarchy.nightlight", "enable" | "disable" | "toggle") => {
            let current = nightlight_enabled();
            let enabling = match method {
                "enable" => true,
                "disable" => false,
                _ => !current.unwrap_or(false),
            };
            let result = run_action(&SystemAction::SetNightlight(enabling));
            match result {
                Ok(()) => Ok(outcome(if enabling { "enabled" } else { "disabled" }, None)),
                Err(_) => Ok(outcome("error", None)),
            }
        }

        ("omarchy.idle", "status" | "debug") => {
            match command_output("omarchy-toggle-idle", &["--status"]) {
                Ok(value) => Ok(outcome(value.trim(), None)),
                Err(_) => Ok(outcome("{}", None)),
            }
        }
        ("omarchy.idle", "enable" | "disable" | "toggle") => {
            let command = match method {
                "enable" => "allow-idle",
                "disable" => "stay-awake",
                _ => "toggle",
            };
            match command_output("omarchy-toggle-idle", &[command]) {
                Ok(value) => Ok(outcome(value.trim(), None)),
                Err(_) => Ok(outcome("error", None)),
            }
        }

        ("omarchy.lock", "isLocked") => {
            let status = lock_status(runtime);
            Ok(outcome(
                if status.get("locked").and_then(serde_json::Value::as_bool) == Some(true) {
                    "true"
                } else {
                    "false"
                },
                None,
            ))
        }
        ("omarchy.lock", "status") => Ok(outcome(&lock_status(runtime).to_string(), None)),
        ("omarchy.lock", "preview") => {
            runtime.lock_preview_visible = true;
            record_lock_event(runtime, "preview");
            Ok(outcome("ok", Some(IpcEvent::Lock { preview: true })))
        }
        ("omarchy.lock", "hidePreview") => {
            runtime.lock_preview_visible = false;
            record_lock_event(runtime, "preview-hidden");
            Ok(outcome("ok", Some(IpcEvent::Lock { preview: false })))
        }
        ("omarchy.lock", "lock") => {
            if !password_pam_configured() {
                record_lock_event(runtime, "lock-denied: missing-pam");
                Ok(outcome("missing-pam", None))
            } else if lock_status(runtime)
                .get("locked")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                Ok(outcome("ok", None))
            } else if !command_present("loginctl") {
                record_lock_event(runtime, "lock-failed: loginctl-missing");
                Ok(outcome("failed", None))
            } else {
                let result = Command::new("loginctl").arg("lock-session").output();
                if result.is_ok_and(|output| output.status.success()) {
                    runtime.lock_requested = true;
                    record_lock_event(runtime, "lock-requested");
                    Ok(outcome("ok", Some(IpcEvent::Lock { preview: false })))
                } else {
                    record_lock_event(runtime, "lock-failed");
                    Ok(outcome("failed", None))
                }
            }
        }

        ("omarchy.dropbox", "status") => Ok(outcome(
            &live_dropbox_status(&runtime.snapshot.omarchy_path),
            None,
        )),
        ("omarchy.tailscale", "status") => Ok(outcome(&live_tailscale_status(), None)),
        ("omarchy.tailscale", "up" | "down" | "toggleTailscale") => {
            let action = if method == "toggleTailscale" {
                if live_tailscale_running() {
                    "down"
                } else {
                    "up"
                }
            } else {
                method
            };
            if command_present("tailscale") {
                let _ = Command::new("tailscale").arg(action).spawn();
            }
            Ok(outcome("ok", None))
        }
        ("omarchy.weather", "edit") => Ok(outcome(
            "ok",
            Some(IpcEvent::Summon {
                id: id.clone(),
                payload: r#"{"edit":true}"#.to_string(),
            }),
        )),

        ("omarchy.image-picker", "preload") => Ok(outcome("ok", None)),
        ("omarchy.image-picker", "cancel") => {
            finish_done_file(arg)?;
            runtime.open_panel_ids.remove(&id);
            Ok(outcome("ok", Some(IpcEvent::Hide { id })))
        }
        _ => Ok(outcome("unknown", None)),
    };
    result
}

fn dispatch_call_lifecycle(
    runtime: &mut IpcRuntime,
    id: &str,
    method: &str,
    arg: &str,
) -> CommandOutcome {
    match method {
        "open" | "show" => {
            let Some(resolved) = summon_panel(runtime, id, arg) else {
                return outcome("unknown", None);
            };
            outcome(
                "ok",
                Some(IpcEvent::Summon {
                    id: resolved,
                    payload: arg.to_string(),
                }),
            )
        }
        "close" | "hide" => {
            runtime.open_panel_ids.remove(id);
            outcome("ok", Some(IpcEvent::Hide { id: id.to_string() }))
        }
        "toggle" => {
            if runtime.open_panel_ids.remove(id) {
                outcome("ok", Some(IpcEvent::Hide { id: id.to_string() }))
            } else {
                let Some(resolved) = summon_panel(runtime, id, arg) else {
                    return outcome("unknown", None);
                };
                outcome(
                    "ok",
                    Some(IpcEvent::Toggle {
                        id: resolved,
                        payload: arg.to_string(),
                    }),
                )
            }
        }
        _ => outcome("unknown", None),
    }
}

fn resolve_call_target(snapshot: &ShellSnapshot, requested: &str) -> Option<String> {
    if snapshot.plugin(requested).is_some() {
        return Some(requested.to_string());
    }
    let alias = match requested {
        "image-selector" => Some("omarchy.image-picker"),
        "background" => Some("omarchy.background"),
        "indicators" => Some("omarchy.indicators"),
        "system-update" => Some("omarchy.system-update"),
        "lock" => Some("omarchy.lock"),
        "notifications" => Some("omarchy.notifications"),
        "osd" => Some("omarchy.osd"),
        "idle" => Some("omarchy.idle"),
        "media" => Some("omarchy.media"),
        "nightlight" => Some("omarchy.nightlight"),
        _ => None,
    };
    alias
        .or_else(|| (!requested.starts_with("omarchy.")).then_some(requested))
        .and_then(|id| {
            if id.starts_with("omarchy.") {
                snapshot.plugin(id).map(|_| id.to_string())
            } else {
                let candidate = format!("omarchy.{id}");
                snapshot.plugin(&candidate).map(|_| candidate)
            }
        })
}

fn call_target_is_loaded(runtime: &IpcRuntime, id: &str) -> bool {
    let Some(plugin) = runtime.snapshot.plugin(id) else {
        return false;
    };
    runtime.open_panel_ids.contains(id)
        || plugin.has_kind("service")
        || plugin
            .raw
            .get("keepLoaded")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        || runtime
            .snapshot
            .left
            .iter()
            .chain(runtime.snapshot.center.iter())
            .chain(runtime.snapshot.right.iter())
            .any(|entry| entry.id == id)
}

fn call_method_supported(id: &str, method: &str) -> bool {
    match id {
        "omarchy.agents" => matches!(
            method,
            "open" | "close" | "show" | "hide" | "toggle" | "refresh" | "next"
        ),
        "omarchy.menu" => matches!(
            method,
            "open" | "close" | "show" | "hide" | "toggle" | "refresh" | "ping"
        ),
        "omarchy.background" => matches!(
            method,
            "refresh" | "set" | "setInstant" | "transition" | "themeTransition"
        ),
        "omarchy.indicators" => method == "refresh",
        "omarchy.system-update" => matches!(method, "refresh" | "clear"),
        "omarchy.lock" => matches!(
            method,
            "lock" | "isLocked" | "status" | "preview" | "hidePreview"
        ),
        "omarchy.notifications" => matches!(
            method,
            "dndState"
                | "toggleDnd"
                | "setDnd"
                | "isDnd"
                | "showHistory"
                | "clear"
                | "dismissAll"
                | "dismissOne"
                | "invokeLast"
                | "dismiss"
                | "ping"
        ),
        "omarchy.osd" => matches!(method, "show" | "close" | "state" | "ping"),
        "omarchy.audio" | "omarchy.bluetooth" | "omarchy.clock" | "omarchy.monitor"
        | "omarchy.network" | "omarchy.power" | "omarchy.tailscale" | "omarchy.weather" => {
            matches!(method, "open" | "close" | "show" | "hide" | "toggle")
                || match id {
                    "omarchy.bluetooth" => method == "toggleBluetooth",
                    "omarchy.clock" => {
                        matches!(method, "refresh" | "cycleFormat" | "toggleWeekStart")
                    }
                    "omarchy.monitor" => matches!(method, "brightness" | "state"),
                    "omarchy.network" => {
                        matches!(method, "toggleNetwork" | "showQr" | "speedTest")
                    }
                    "omarchy.power" => method == "togglePercentage",
                    "omarchy.tailscale" => {
                        matches!(
                            method,
                            "refresh" | "up" | "down" | "toggleTailscale" | "status"
                        )
                    }
                    "omarchy.weather" => matches!(method, "refresh" | "edit"),
                    "omarchy.audio" => false,
                    _ => false,
                }
        }
        "omarchy.dropbox" => matches!(
            method,
            "open" | "close" | "show" | "hide" | "toggle" | "refresh" | "login" | "status"
        ),
        "omarchy.media" => matches!(
            method,
            "status"
                | "playPause"
                | "next"
                | "previous"
                | "play"
                | "pause"
                | "sourceNext"
                | "sourcePrevious"
                | "sourceSwitch"
                | "sourceSwitchPrevious"
                | "ping"
        ),
        "omarchy.nightlight" => {
            matches!(
                method,
                "status" | "refresh" | "enable" | "disable" | "toggle"
            )
        }
        "omarchy.idle" => matches!(method, "status" | "debug" | "enable" | "disable" | "toggle"),
        "omarchy.image-picker" => {
            matches!(method, "open" | "close" | "preload" | "cancel" | "ping")
        }
        "omarchy.clipboard"
        | "omarchy.emojis"
        | "omarchy.reminders"
        | "omarchy.dev-gallery"
        | "omarchy.disk-speedtest"
        | "omarchy.speedtest"
        | "omarchy.wifiqr" => {
            matches!(method, "open" | "close" | "toggle")
        }
        _ => false,
    }
}

fn parse_bool_arg(arg: &str) -> bool {
    matches!(
        arg.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "on" | "yes"
    )
}

fn required_call_arg<'a>(args: &'a [String], index: usize, label: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing {label}"))
}

fn parse_brightness_arg(arg: &str) -> u8 {
    arg.trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value.round().clamp(1.0, 100.0) as u8)
        .unwrap_or(1)
}

fn snapshot_widget_bool(snapshot: &ShellSnapshot, id: &str, key: &str) -> bool {
    snapshot
        .left
        .iter()
        .chain(snapshot.center.iter())
        .chain(snapshot.right.iter())
        .find(|entry| entry.id == id)
        .and_then(|entry| entry.settings.get(key))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn media_action(action: SystemAction) -> Result<CommandOutcome, String> {
    Ok(outcome(
        if run_action(&action).is_ok() {
            "ok"
        } else {
            "unhandled"
        },
        None,
    ))
}

fn media_player_action(player: &str, action: &str) -> Result<(), String> {
    validate_ipc_argument(player, "media player")?;
    if !matches!(action, "play" | "pause" | "play-pause") {
        return Err("invalid media action".to_string());
    }
    let method = match action {
        "play" => "Play",
        "pause" => "Pause",
        "play-pause" => "PlayPause",
        _ => unreachable!(),
    };
    if player.starts_with("org.mpris.MediaPlayer2.") {
        dbus::call_player(player, method)
    } else {
        command_output("playerctl", &["--player", player, action]).map(|_| ())
    }
}

fn validate_ipc_argument(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        Err(format!("invalid {label}"))
    } else {
        Ok(())
    }
}

fn nightlight_status_json() -> String {
    if let Ok(raw) = command_output("omarchy-toggle-nightlight", &["--status"])
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
        && value.is_object()
    {
        return value.to_string();
    }
    let nightlight = SystemSnapshot::collect().nightlight;
    serde_json::json!({
        "enabled": nightlight.active,
        "temperature": nightlight.temperature,
    })
    .to_string()
}

fn nightlight_enabled() -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(&nightlight_status_json())
        .ok()
        .and_then(|value| value.get("enabled").and_then(serde_json::Value::as_bool))
}

fn lock_status(runtime: &IpcRuntime) -> serde_json::Value {
    let session_locked = session_is_locked();
    let real_screens = real_screen_count();
    let password_pam = password_pam_configured();
    let fingerprint = fingerprint_configured();
    serde_json::json!({
        "locked": runtime.lock_requested || session_locked,
        "requested": runtime.lock_requested,
        "pending": runtime.lock_requested && !session_locked,
        "sessionLocked": session_locked,
        "secure": session_locked,
        "realScreens": real_screens,
        "passwordPam": password_pam,
        "fingerprint": fingerprint,
        "authenticating": false,
        "lastEvent": runtime.lock_last_event,
        "lastEventAt": runtime.lock_last_event_at,
        "preview": runtime.lock_preview_visible,
    })
}

fn record_lock_event(runtime: &mut IpcRuntime, event: &str) {
    runtime.lock_last_event = event.to_string();
    runtime.lock_last_event_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
}

fn session_is_locked() -> bool {
    if let Some(session) = std::env::var_os("XDG_SESSION_ID").filter(|value| !value.is_empty())
        && let Ok(output) = Command::new("loginctl")
            .args([
                "show-session",
                &session.to_string_lossy(),
                "-p",
                "LockedHint",
                "--value",
            ])
            .output()
        && output.status.success()
    {
        let value = String::from_utf8_lossy(&output.stdout);
        return parse_bool_arg(value.trim());
    }
    Command::new("omarchy-hyprland-session-locked")
        .status()
        .is_ok_and(|status| status.success())
}

fn real_screen_count() -> usize {
    let Ok(raw) = command_output("hyprctl", &["-j", "monitors"]) else {
        return 0;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .map(|monitors| {
            monitors
                .iter()
                .filter(|monitor| {
                    monitor
                        .get("width")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0)
                        > 0
                        && monitor
                            .get("height")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(0)
                            > 0
                })
                .count()
        })
        .unwrap_or(0)
}

fn password_pam_configured() -> bool {
    pam_configured_at(Path::new("/etc/pam.d/omarchy-lock-password"))
}

fn pam_configured_at(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
        && fs::read(path).is_ok_and(|contents| !contents.is_empty())
}

fn fingerprint_configured() -> bool {
    let path = Path::new("/etc/pam.d/omarchy-lock-fingerprint");
    if !pam_configured_at(path) || !command_present("fprintd-list") {
        return false;
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();
    if user.is_empty() {
        return false;
    }
    Command::new("fprintd-list")
        .arg(&user)
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .to_ascii_lowercase()
                    .contains("finger")
        })
}

fn notifications_dir(home: &Path) -> PathBuf {
    home.join(".local/state/omarchy/notifications")
}

fn notification_files(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "read notifications {}: {error}",
                directory.display()
            ));
        }
    };
    let mut files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn read_notification_history(home: &Path) -> Result<String, String> {
    let directory = notifications_dir(home).join("history");
    let mut values = notification_files(&directory)?
        .into_iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .collect::<Vec<_>>();
    values.reverse();
    Ok(serde_json::Value::Array(values).to_string())
}

fn clear_notification_history(home: &Path) -> Result<(), String> {
    let history = notifications_dir(home).join("history");
    let images = notifications_dir(home).join("images");
    for path in notification_files(&history)? {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        fs::remove_file(&path).map_err(|error| format!("remove notification history: {error}"))?;
        remove_notification_images(&images, stem)?;
    }
    Ok(())
}

fn clear_live_notifications(home: &Path) -> Result<(), String> {
    let directory = notifications_dir(home);
    let images = directory.join("images");
    for path in notification_files(&directory)? {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        fs::remove_file(&path).map_err(|error| format!("remove live notification: {error}"))?;
        remove_notification_images(&images, stem)?;
    }
    Ok(())
}

fn remove_latest_live_notification(home: &Path) -> Result<bool, String> {
    let directory = notifications_dir(home);
    let Some(path) = notification_files(&directory)?.into_iter().next_back() else {
        return Ok(false);
    };
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    fs::remove_file(&path).map_err(|error| format!("remove live notification: {error}"))?;
    remove_notification_images(&directory.join("images"), stem)?;
    Ok(true)
}

fn invoke_latest_live_notification(home: &Path) -> Result<bool, String> {
    let directory = notifications_dir(home);
    let Some(path) = notification_files(&directory)?.into_iter().next_back() else {
        return Ok(false);
    };
    let value = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or_default();
    let invoked = value
        .get("execArgv")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_exec_argv)
        .is_some_and(|argv| spawn_argv(&argv).is_ok());
    if !invoked
        && let Some(app) = value.get("app").and_then(serde_json::Value::as_str)
        && !app.is_empty()
    {
        let _ = Command::new("omarchy-hyprland-focus-app").arg(app).spawn();
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    fs::remove_file(&path).map_err(|error| format!("remove invoked notification: {error}"))?;
    remove_notification_images(&directory.join("images"), stem)?;
    Ok(true)
}

fn dismiss_notifications_matching(home: &Path, needle: &str) -> Result<bool, String> {
    let directory = notifications_dir(home);
    let images = directory.join("images");
    let mut removed = false;
    for path in notification_files(&directory)? {
        let Some(summary) = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|value| {
                value
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        else {
            continue;
        };
        if summary.contains(needle) {
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            fs::remove_file(&path).map_err(|error| format!("remove notification: {error}"))?;
            remove_notification_images(&images, stem)?;
            removed = true;
        }
    }
    Ok(removed)
}

fn remove_notification_images(images: &Path, stem: &str) -> Result<(), String> {
    if stem.is_empty() {
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(images) else {
        return Ok(());
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with(&format!("{stem}-")))
        {
            fs::remove_file(path).map_err(|error| format!("remove notification image: {error}"))?;
        }
    }
    Ok(())
}

fn parse_exec_argv(raw: &str) -> Option<Vec<String>> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let argv = value
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let program = argv.first()?;
    (!program.is_empty() && !program.starts_with('-') && !program.chars().any(char::is_control))
        .then_some(argv)
}

fn spawn_argv(argv: &[String]) -> Result<(), String> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| "empty notification action".to_string())?;
    Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("notification action: {error}"))
}

fn outcome(output: &str, event: Option<IpcEvent>) -> CommandOutcome {
    CommandOutcome {
        output: output.to_string(),
        event,
    }
}

fn resolve_enabled_id(snapshot: &ShellSnapshot, requested: &str) -> String {
    let key = requested.to_string();
    snapshot
        .plugins
        .iter()
        .filter(|plugin| {
            plugin
                .raw
                .get("omarchy")
                .and_then(|metadata| metadata.get("clonedFrom"))
                .and_then(serde_json::Value::as_str)
                == Some(key.as_str())
                && snapshot.plugin_is_enabled(&plugin.id)
        })
        .map(|plugin| plugin.id.clone())
        .min()
        .unwrap_or(key)
}

fn summon_panel(runtime: &mut IpcRuntime, requested: &str, payload: &str) -> Option<String> {
    let resolved = resolve_enabled_id(&runtime.snapshot, requested);
    let plugin = runtime.snapshot.plugin(&resolved)?;
    if !runtime.snapshot.plugin_is_enabled(&resolved) {
        return None;
    }
    runtime.open_panel_ids.insert(resolved.clone());
    runtime
        .pending_payloads
        .entry(resolved.clone())
        .or_default()
        .push(payload.to_string());
    let _ = plugin;
    Some(resolved)
}

fn toggle_panel(runtime: &mut IpcRuntime, id: &str, payload: &str) -> Option<bool> {
    if runtime.open_panel_ids.remove(id) {
        return Some(false);
    }
    summon_panel(runtime, id, payload).map(|_| true)
}

fn panel_widget_id_at(snapshot: &ShellSnapshot, section: &str, index: &str) -> Option<String> {
    let requested = index.parse::<f64>().ok()?.round();
    if requested < 1.0 || !requested.is_finite() {
        return None;
    }
    let entries = match section {
        "left" => &snapshot.left,
        "center" => &snapshot.center,
        "right" => &snapshot.right,
        _ => return None,
    };
    entries
        .iter()
        .filter(|entry| {
            snapshot
                .plugin(&entry.id)
                .is_some_and(|plugin| plugin.entry_points.contains_key("barWidget"))
        })
        .nth((requested as usize).saturating_sub(1))
        .map(|entry| entry.id.clone())
}

fn plugin_list(snapshot: &ShellSnapshot) -> String {
    let mut plugins = snapshot
        .plugins
        .iter()
        .map(|plugin| {
            let active = plugin.has_kind("bar") && snapshot.plugin_is_enabled(&plugin.id);
            let cloned_from = plugin
                .raw
                .get("omarchy")
                .and_then(|metadata| metadata.get("clonedFrom"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            serde_json::json!({
                "id": plugin.id,
                "name": plugin.name,
                "kinds": plugin.kinds,
                "enabled": if plugin.has_kind("bar") { active } else if plugin.has_kind("bar-widget") { snapshot.left.iter().chain(snapshot.center.iter()).chain(snapshot.right.iter()).any(|entry| entry.id == plugin.id) } else { snapshot.plugin_is_enabled(&plugin.id) },
                "active": active,
                "canDisable": !plugin.has_kind("bar"),
                "firstParty": plugin.source == crate::config::PluginSource::FirstParty,
                "clonedFrom": cloned_from,
            })
        })
        .collect::<Vec<_>>();
    plugins.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["name"].as_str().unwrap_or_default())
            .then_with(|| {
                left["id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["id"].as_str().unwrap_or_default())
            })
    });
    serde_json::to_string(&plugins).expect("plugin list is serializable")
}

fn parse_json_object(raw: &str, label: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid {label}: {error}"))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(format!("invalid {label}: expected an object"))
    }
}

fn decode_base64(raw: &str) -> Vec<u8> {
    // Keep this dependency-free: the shell's theme payloads are optional for
    // the host command path, and invalid input intentionally becomes empty.
    let mut bytes = Vec::new();
    let mut current = 0u32;
    let mut bits = 0u8;
    for value in raw.bytes().filter_map(base64_value) {
        current = (current << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(((current >> bits) & 0xff) as u8);
        }
    }
    bytes
}

fn decode_utf8(raw: String) -> String {
    String::from_utf8_lossy(&decode_base64(&raw)).into_owned()
}

fn command_output(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("{program} exited with {}", output.status)
        } else {
            format!("{program}: {detail}")
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn live_dropbox_status(omarchy_path: &Path) -> String {
    let helper = omarchy_path.join("shell/plugins/panels/dropbox/status.py");
    if !helper.is_file() {
        return "Unavailable".to_string();
    }
    let output = match Command::new("python3").arg(&helper).arg("25").output() {
        Ok(output) => output,
        Err(_) => return "Unavailable".to_string(),
    };
    if !output.status.success() {
        return "Unavailable".to_string();
    }
    let state = parse_dropbox_status(&String::from_utf8_lossy(&output.stdout));
    if state.status_text.is_empty() {
        "Unavailable".to_string()
    } else {
        state.status_text
    }
}

fn live_tailscale_state() -> Option<crate::plugins::TailscaleState> {
    let output = Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_tailscale_status(&String::from_utf8_lossy(&output.stdout)))
}

fn live_tailscale_status() -> String {
    match live_tailscale_state() {
        Some(state) if !state.status.is_empty() => state.status,
        Some(_) => "Disconnected".to_string(),
        None if command_present("tailscale") => "Unavailable".to_string(),
        None => "Not installed".to_string(),
    }
}

fn live_tailscale_running() -> bool {
    live_tailscale_state().is_some_and(|state| state.running)
}

fn command_present(program: &str) -> bool {
    Command::new("which")
        .arg(program)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn finish_done_file(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Ok(());
    }
    let target = PathBuf::from(path);
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&target)
        .map(|_| ())
        .map_err(|error| format!("finish image selector file {}: {error}", target.display()))
}

fn current_background_path(home: &Path) -> String {
    let path = home.join(".local/state/omarchy/current/background");
    fs::canonicalize(path)
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn home_from_config_path(path: &Path) -> PathBuf {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        })
}

fn load_dnd_state(path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| value.get("dnd").cloned())
        .map(|value| match value {
            serde_json::Value::Bool(value) => value,
            serde_json::Value::String(value) => parse_bool_arg(&value),
            _ => false,
        })
        .unwrap_or(false)
}

fn persist_dnd_state(path: &Path, enabled: bool) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("notifications state has no parent".to_string());
    };
    fs::create_dir_all(parent).map_err(|error| format!("create notifications state: {error}"))?;
    let temporary = parent.join(format!("notifications.json.tmp-{}", std::process::id()));
    let contents = serde_json::json!({"version": 3, "dnd": enabled}).to_string() + "\n";
    fs::write(&temporary, contents)
        .map_err(|error| format!("write notifications state: {error}"))?;
    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("replace notifications state: {error}")
    })
}

fn base64_value(value: u8) -> Option<u8> {
    match value {
        b'A'..=b'Z' => Some(value - b'A'),
        b'a'..=b'z' => Some(value - b'a' + 26),
        b'0'..=b'9' => Some(value - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn required(args: &[String], index: usize, label: &str) -> Result<String, String> {
    args.get(index)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| format!("missing {label}"))
}

fn positional(args: &[String], index: usize, label: &str) -> Result<String, String> {
    args.get(index)
        .cloned()
        .ok_or_else(|| format!("missing {label}"))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{
        IpcEvent, IpcRuntime, ShellCommand, clear_live_notifications, clear_notification_history,
        dismiss_notifications_matching, dispatch_runtime, load_dnd_state, parse, parse_exec_argv,
        persist_dnd_state, read_notification_history,
    };
    use crate::config::ShellSnapshot;

    #[test]
    fn parses_omarchy_shell_commands() {
        assert_eq!(parse(&args(&["shell", "ping"])), Ok(ShellCommand::Ping));
        assert_eq!(
            parse(&args(&["shell", "toggle", "omarchy.menu"])),
            Ok(ShellCommand::Toggle {
                id: "omarchy.menu".to_string(),
                payload: "{}".to_string(),
            })
        );
        assert_eq!(
            parse(&args(&["shell", "setPluginEnabled", "omarchy.clock"])),
            Ok(ShellCommand::SetPluginEnabled {
                id: "omarchy.clock".to_string(),
                enabled: false,
            })
        );
    }

    #[test]
    fn parses_direct_service_and_image_selector_targets() {
        assert_eq!(
            parse(&args(&["lock", "lock"])),
            Ok(ShellCommand::Call {
                id: "lock".to_string(),
                method: "lock".to_string(),
                args: Vec::new(),
            })
        );

        let parsed = parse(&args(&[
            "image-selector",
            "open",
            "/tmp/images",
            "SGVsbG8=",
            "selected.png",
            "/tmp/selection",
            "/tmp/done",
            "true",
            "false",
        ]))
        .unwrap();
        let ShellCommand::ImageSelectorOpen { payload } = parsed else {
            panic!("expected image selector open");
        };
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["imageDirs"], "/tmp/images");
        assert_eq!(payload["imageRows"], "Hello");
        assert_eq!(payload["filterable"], "false");

        let background = parse(&args(&[
            "background",
            "themeTransition",
            "/tmp/old.png",
            "/tmp/incoming.png",
            "/tmp/final.png",
            "Y29sb3Jz",
            "c2hlbGw=",
        ]))
        .unwrap();
        assert_eq!(
            background,
            ShellCommand::Call {
                id: "background".to_string(),
                method: "themeTransition".to_string(),
                args: vec![
                    "/tmp/old.png".to_string(),
                    "/tmp/incoming.png".to_string(),
                    "/tmp/final.png".to_string(),
                    "Y29sb3Jz".to_string(),
                    "c2hlbGw=".to_string(),
                ],
            }
        );
    }

    #[test]
    fn rejects_unknown_methods() {
        assert!(parse(&args(&["shell", "nope"])).is_err());
    }

    #[test]
    fn notification_dnd_state_round_trips_atomically() {
        let root = std::env::temp_dir().join(format!(
            "omarchy-gpui-dnd-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let path = root.join("notifications.json");
        fs::create_dir_all(&root).expect("create dnd fixture directory");
        fs::write(&path, r#"{"version":3,"dnd":true}"#).expect("write dnd fixture");
        assert!(load_dnd_state(&path));
        persist_dnd_state(&path, false).expect("persist dnd state");
        assert!(!load_dnd_state(&path));
        assert!(
            fs::read_to_string(&path)
                .expect("read dnd state")
                .contains("\"dnd\":false")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn notification_history_and_live_actions_use_reference_state_layout() {
        let root = std::env::temp_dir().join(format!(
            "omarchy-gpui-notifications-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let live = root.join("home/.local/state/omarchy/notifications");
        let history = live.join("history");
        fs::create_dir_all(&history).expect("create notification fixture");
        fs::write(
            history.join("100-1.json"),
            r#"{"summary":"older","body":"one"}"#,
        )
        .expect("write old notification");
        fs::write(
            history.join("200-2.json"),
            r#"{"summary":"newer","body":"two"}"#,
        )
        .expect("write new notification");
        fs::write(
            live.join("300-3.json"),
            r#"{"summary":"live","body":"three"}"#,
        )
        .expect("write live notification");

        let history_json: serde_json::Value =
            serde_json::from_str(&read_notification_history(&root.join("home")).unwrap())
                .expect("parse history");
        assert_eq!(history_json[0]["summary"], "newer");
        assert_eq!(history_json[1]["summary"], "older");
        assert!(dismiss_notifications_matching(&root.join("home"), "live").unwrap());
        assert!(!live.join("300-3.json").exists());

        fs::write(live.join("400-4.json"), r#"{"summary":"another live"}"#)
            .expect("write second live notification");
        clear_live_notifications(&root.join("home")).expect("clear live notifications");
        assert!(!live.join("400-4.json").exists());
        clear_notification_history(&root.join("home")).expect("clear notification history");
        assert!(!history.join("100-1.json").exists());
        assert!(!history.join("200-2.json").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn notification_exec_argv_is_structurally_validated() {
        assert_eq!(
            parse_exec_argv(r#"["notify-send","hello"]"#),
            Some(vec!["notify-send".to_string(), "hello".to_string()])
        );
        assert!(parse_exec_argv(r#"[]"#).is_none());
        assert!(parse_exec_argv(r#"["-c","unsafe"]"#).is_none());
        assert!(parse_exec_argv(r#"["notify-send",3]"#).is_none());
    }

    #[test]
    fn void_methods_and_panel_index_follow_reference_contract() {
        let (root, snapshot) = fixture();
        let mut runtime = IpcRuntime::new(snapshot);

        let rescan = parse(&args(&["shell", "rescanPlugins"])).unwrap();
        let result = dispatch_runtime(&rescan, &mut runtime).unwrap();
        assert_eq!(result.output, "");
        assert_eq!(result.event, Some(IpcEvent::Refresh));

        let toggle_at = parse(&args(&["shell", "togglePanelAt", "left", "1"])).unwrap();
        let result = dispatch_runtime(&toggle_at, &mut runtime).unwrap();
        assert_eq!(result.output, "omarchy.audio");
        assert_eq!(
            result.event,
            Some(IpcEvent::Toggle {
                id: "omarchy.audio".to_string(),
                payload: "{}".to_string(),
            })
        );

        let zero_index = parse(&args(&["shell", "togglePanelAt", "left", "0"])).unwrap();
        assert_eq!(
            dispatch_runtime(&zero_index, &mut runtime).unwrap().output,
            "unknown"
        );

        let hide = parse(&args(&["shell", "hide", "omarchy.audio"])).unwrap();
        assert_eq!(dispatch_runtime(&hide, &mut runtime).unwrap().output, "");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn summon_rejects_unknown_plugins_without_emitting_an_event() {
        let (root, snapshot) = fixture();
        let mut runtime = IpcRuntime::new(snapshot);
        let summon = parse(&args(&["shell", "summon", "missing.plugin"])).unwrap();
        let result = dispatch_runtime(&summon, &mut runtime).unwrap();
        assert_eq!(result.output, "unknown");
        assert_eq!(result.event, None);
        assert!(runtime.open_panel_ids.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn call_dispatches_loaded_lifecycle_and_plugin_state_handlers() {
        let (root, snapshot) = fixture();
        let mut runtime = IpcRuntime::new(snapshot);

        let open = parse(&args(&["shell", "call", "omarchy.audio", "open"])).unwrap();
        let result = dispatch_runtime(&open, &mut runtime).unwrap();
        assert_eq!(result.output, "ok");
        assert_eq!(
            result.event,
            Some(IpcEvent::Summon {
                id: "omarchy.audio".to_string(),
                payload: String::new(),
            })
        );

        let close = parse(&args(&["shell", "call", "omarchy.audio", "close"])).unwrap();
        let result = dispatch_runtime(&close, &mut runtime).unwrap();
        assert_eq!(result.output, "ok");
        assert_eq!(
            result.event,
            Some(IpcEvent::Hide {
                id: "omarchy.audio".to_string(),
            })
        );

        let dnd_state = parse(&args(&["shell", "call", "notifications", "dndState"])).unwrap();
        assert_eq!(
            dispatch_runtime(&dnd_state, &mut runtime).unwrap().output,
            "off"
        );
        let toggle_dnd = parse(&args(&["shell", "call", "notifications", "toggleDnd"])).unwrap();
        assert_eq!(
            dispatch_runtime(&toggle_dnd, &mut runtime).unwrap().output,
            "on"
        );
        let set_dnd = parse(&args(&["shell", "call", "notifications", "setDnd", "no"])).unwrap();
        assert_eq!(
            dispatch_runtime(&set_dnd, &mut runtime).unwrap().output,
            "off"
        );

        let background = parse(&args(&[
            "shell",
            "call",
            "background",
            "transition",
            "/tmp/old.png",
            "/tmp/new.png",
        ]))
        .unwrap();
        let result = dispatch_runtime(&background, &mut runtime).unwrap();
        assert_eq!(result.output, "");
        assert_eq!(
            result.event,
            Some(IpcEvent::Background {
                path: "/tmp/new.png".to_string(),
                instant: false,
            })
        );

        let unknown = parse(&args(&["shell", "call", "omarchy.audio", "nope"])).unwrap();
        assert_eq!(
            dispatch_runtime(&unknown, &mut runtime).unwrap().output,
            "unknown"
        );
        let _ = fs::remove_dir_all(root);
    }

    fn fixture() -> (PathBuf, ShellSnapshot) {
        let root = std::env::temp_dir().join(format!(
            "omarchy-gpui-ipc-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let omarchy = root.join("omarchy");
        let home = root.join("home");
        fs::create_dir_all(omarchy.join("config/omarchy")).expect("create config fixture");
        fs::write(
            omarchy.join("config/omarchy/shell.json"),
            r#"{"version":1,"bar":{"layout":{"left":[{"id":"omarchy.audio"}],"center":[{"id":"omarchy.clock"}],"right":[{"id":"omarchy.tray"}]}},"plugins":[]}"#,
        )
        .expect("write config fixture");
        for (id, name) in [("omarchy.audio", "Audio"), ("omarchy.clock", "Clock")] {
            let directory = omarchy.join(format!("shell/plugins/{id}"));
            fs::create_dir_all(&directory).expect("create plugin fixture");
            fs::write(
                directory.join("manifest.json"),
                format!(
                    r#"{{"schemaVersion":1,"id":"{id}","name":"{name}","version":"1.0.0","kinds":["bar-widget"],"entryPoints":{{"barWidget":"Panel.qml"}}}}"#
                ),
            )
            .expect("write plugin fixture");
        }
        let directory = omarchy.join("shell/plugins/omarchy.background");
        fs::create_dir_all(&directory).expect("create background fixture");
        fs::write(
            directory.join("manifest.json"),
            r#"{"schemaVersion":1,"id":"omarchy.background","name":"Background","version":"1.0.0","kinds":["service"],"entryPoints":{"service":"Background.qml"}}"#,
        )
        .expect("write background fixture");
        let directory = omarchy.join("shell/plugins/omarchy.notifications");
        fs::create_dir_all(&directory).expect("create notifications fixture");
        fs::write(
            directory.join("manifest.json"),
            r#"{"schemaVersion":1,"id":"omarchy.notifications","name":"Notifications","version":"1.0.0","kinds":["service"],"entryPoints":{"service":"Service.qml"}}"#,
        )
        .expect("write notifications fixture");
        (
            root.clone(),
            ShellSnapshot::load_from_paths(&omarchy, &home),
        )
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }
}
