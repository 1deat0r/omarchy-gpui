use std::{
    fs,
    io::{Read, Write},
    net::Shutdown,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::config::ShellSnapshot;

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
        arg: String,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IpcEvent {
    Refresh,
    Summon { id: String, payload: String },
    Hide { id: String },
    Toggle { id: String, payload: String },
}

pub type IpcEventReceiver = Arc<Mutex<Receiver<IpcEvent>>>;

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
        let state = Arc::new(Mutex::new(snapshot));
        let (event_tx, event_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("omarchy-gpui-ipc".to_string())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => handle_connection(stream, &state, &event_tx),
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
                thread: Some(thread),
            },
            Arc::new(Mutex::new(event_rx)),
        ))
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
    state: &Arc<Mutex<ShellSnapshot>>,
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
    state: &Arc<Mutex<ShellSnapshot>>,
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

    let parsed = parse(&args);
    let event = parsed.as_ref().ok().and_then(event_for_command);
    let result = state
        .lock()
        .map_err(|_| "IPC state lock poisoned".to_string())
        .and_then(|mut snapshot| parsed.and_then(|command| dispatch(&command, &mut snapshot)));
    match result {
        Ok(output) => (serde_json::json!({"ok": true, "output": output}), event),
        Err(error) => (serde_json::json!({"ok": false, "error": error}), None),
    }
}

fn event_for_command(command: &ShellCommand) -> Option<IpcEvent> {
    match command {
        ShellCommand::ReloadConfig
        | ShellCommand::RescanPlugins
        | ShellCommand::ToggleBarTransparency
        | ShellCommand::EnablePlugin { .. }
        | ShellCommand::PutBarWidget { .. }
        | ShellCommand::MoveBarWidget { .. }
        | ShellCommand::SetBarWidget { .. }
        | ShellCommand::SetPluginEnabled { .. } => Some(IpcEvent::Refresh),
        ShellCommand::Summon { id, payload } => Some(IpcEvent::Summon {
            id: id.clone(),
            payload: payload.clone(),
        }),
        ShellCommand::Hide { id } => Some(IpcEvent::Hide { id: id.clone() }),
        ShellCommand::Toggle { id, payload } => Some(IpcEvent::Toggle {
            id: id.clone(),
            payload: payload.clone(),
        }),
        _ => None,
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
        return Err("expected the `shell` target".to_string());
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
            arg: args.get(4).cloned().unwrap_or_else(|| "".to_string()),
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

pub fn dispatch(command: &ShellCommand, snapshot: &mut ShellSnapshot) -> Result<String, String> {
    match command {
        ShellCommand::Ping => Ok("ok".to_string()),
        ShellCommand::ApplyTheme { colors, shell } => {
            // GPUI theme application is process-local for now. Decode both
            // payloads here so malformed base64 follows the same fail-open
            // contract as the reference shell while the theme adapter is
            // wired into the renderer.
            let _ = decode_base64(colors);
            let _ = decode_base64(shell);
            Ok("ok".to_string())
        }
        ShellCommand::ReloadConfig | ShellCommand::RescanPlugins => {
            snapshot.reload();
            Ok("ok".to_string())
        }
        ShellCommand::ToggleBarTransparency => {
            snapshot.toggle_bar_transparency()?;
            Ok("ok".to_string())
        }
        ShellCommand::EnablePlugin { id, placement } => {
            let placement = parse_json_object(placement, "placement")?;
            if snapshot.set_plugin_enabled(id, true, Some(&placement))? {
                Ok("ok".to_string())
            } else {
                Ok("unknown".to_string())
            }
        }
        ShellCommand::PutBarWidget { id, placement } => {
            let placement = parse_json_object(placement, "placement")?;
            let error = snapshot.put_bar_widget(id, Some(&placement))?;
            Ok(if error.is_empty() {
                "ok".to_string()
            } else {
                error
            })
        }
        ShellCommand::MoveBarWidget { id, placement } => {
            let placement = parse_json_object(placement, "placement")?;
            let error = snapshot.move_bar_widget(id, &placement)?;
            Ok(if error.is_empty() {
                "ok".to_string()
            } else {
                error
            })
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
            Ok(if error.is_empty() {
                "ok".to_string()
            } else {
                error
            })
        }
        ShellCommand::ListPlugins => Ok(plugin_list(snapshot)),
        ShellCommand::ListShellConfig => Ok(snapshot.config.to_string()),
        ShellCommand::DebugBarGeometry => Ok("[]".to_string()),
        ShellCommand::TogglePanelAt { section, index } => {
            let Some(index) = index.parse::<usize>().ok() else {
                return Ok("unknown".to_string());
            };
            let entries = match section.as_str() {
                "left" => &snapshot.left,
                "center" => &snapshot.center,
                "right" => &snapshot.right,
                _ => return Ok("unknown".to_string()),
            };
            Ok(entries
                .get(index)
                .map(|entry| entry.id.clone())
                .unwrap_or_else(|| "unknown".to_string()))
        }
        ShellCommand::Call { .. } => Ok("unknown".to_string()),
        ShellCommand::Summon { id, .. }
        | ShellCommand::Hide { id }
        | ShellCommand::Toggle { id, .. } => {
            if snapshot.plugin(id).is_some() {
                Ok("ok".to_string())
            } else {
                Ok("unknown".to_string())
            }
        }
        ShellCommand::SetPluginEnabled { id, enabled } => {
            if snapshot.set_plugin_enabled(id, *enabled, None)? {
                Ok("ok".to_string())
            } else {
                Ok("unknown".to_string())
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{ShellCommand, parse};

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
    fn rejects_unknown_methods() {
        assert!(parse(&args(&["shell", "nope"])).is_err());
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }
}
