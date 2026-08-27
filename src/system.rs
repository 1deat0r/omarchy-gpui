//! Read-only adapters for the services the Omarchy shell exposes in its bar.
//!
//! The reference shell gets these values from Quickshell integrations.  The
//! GPUI port keeps the first adapter layer deliberately small and explicit:
//! each adapter owns the command it talks to, parses a stable text/JSON
//! boundary, and records an error instead of turning an unavailable service
//! into a fabricated value.  The renderer can therefore keep running while a
//! service is missing, and the same parsers can be exercised with fixtures.

use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use crate::dbus;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemSnapshot {
    pub hyprland: HyprlandState,
    pub audio: AudioState,
    pub network: NetworkState,
    pub bluetooth: BluetoothState,
    pub battery: BatteryState,
    pub media: MediaState,
    pub display: DisplayState,
    pub power: PowerState,
    pub resources: ResourceState,
    pub nightlight: NightlightState,
    pub collected_at: u64,
}

impl SystemSnapshot {
    pub fn collect() -> Self {
        Self {
            hyprland: HyprlandState::collect(),
            audio: AudioState::collect(),
            network: NetworkState::collect(),
            bluetooth: BluetoothState::collect(),
            battery: BatteryState::collect(),
            media: MediaState::collect(),
            display: DisplayState::collect(),
            power: PowerState::collect(),
            resources: ResourceState::collect(),
            nightlight: NightlightState::collect(),
            collected_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

/// Subscribe to Hyprland's raw event socket. The reference shell receives
/// these events from Quickshell and uses them to update active windows,
/// workspaces, monitors, and service state without waiting for a polling
/// timer. The reconnect loop intentionally treats compositor restarts as a
/// normal lifecycle event.
pub fn subscribe_hyprland_events() -> Option<Receiver<String>> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let socket = runtime.join("hypr").join(signature).join(".socket2.sock");
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("omarchy-gpui-hyprland-events".to_string())
        .spawn(move || {
            loop {
                let stream = match std::os::unix::net::UnixStream::connect(&socket) {
                    Ok(stream) => stream,
                    Err(_) => {
                        thread::sleep(std::time::Duration::from_millis(250));
                        continue;
                    }
                };
                let reader = BufReader::new(stream);
                for line in reader.lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    if sender.send(line).is_err() {
                        return;
                    }
                }
                thread::sleep(std::time::Duration::from_millis(100));
            }
        })
        .ok()?;
    Some(receiver)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemAction {
    FocusWorkspace(String),
    ToggleOutputMute,
    ToggleInputMute,
    SetOutputVolume(u8),
    SetAudioNodeVolume {
        id: u32,
        percent: u8,
    },
    ToggleAudioNodeMute {
        id: u32,
    },
    SetDefaultAudioSink {
        id: u32,
        name: String,
    },
    SetDefaultAudioSource {
        id: u32,
        name: String,
    },
    MediaPlayPause,
    MediaPlay,
    MediaPause,
    MediaNext,
    MediaPrevious,
    SetWifiEnabled(bool),
    SetBluetoothPower(bool),
    ActivateNetwork(String),
    SetBrightness {
        monitor: String,
        percent: u8,
    },
    ToggleDisplay {
        name: String,
        enabled: bool,
    },
    SetMonitorScale(String),
    SetTextSize(u8),
    SetPowerProfile {
        profile: String,
        on_battery: bool,
    },
    SetNetworkBand(String),
    SetNightlight(bool),
    BluetoothDevice {
        action: BluetoothDeviceAction,
        address: String,
    },
    ConnectNetwork {
        ssid: String,
        device: String,
    },
    DisconnectNetwork(String),
    ForgetNetwork(String),
    RescanWifi(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BluetoothDeviceAction {
    Pair,
    Connect,
    Disconnect,
    Forget,
}

pub fn run_action(action: &SystemAction) -> Result<(), String> {
    match action {
        SystemAction::FocusWorkspace(workspace) => {
            validate_argument(workspace, "workspace")?;
            command("hyprctl", &["dispatch", "workspace", workspace]).map(|_| ())
        }
        SystemAction::ToggleOutputMute => {
            command("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]).map(|_| ())
        }
        SystemAction::ToggleInputMute => {
            command("wpctl", &["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"]).map(|_| ())
        }
        SystemAction::SetOutputVolume(percent) => {
            let value = format!("{:.2}", f32::from(*percent) / 100.0);
            command("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &value]).map(|_| ())
        }
        SystemAction::SetAudioNodeVolume { id, percent } => {
            let id = id.to_string();
            let value = format!("{:.2}", f32::from(*percent) / 100.0);
            command("wpctl", &["set-volume", &id, &value]).map(|_| ())
        }
        SystemAction::ToggleAudioNodeMute { id } => {
            let id = id.to_string();
            command("wpctl", &["set-mute", &id, "toggle"]).map(|_| ())
        }
        SystemAction::SetDefaultAudioSink { id, name } => {
            validate_argument(name, "audio sink")?;
            let id = id.to_string();
            command("omarchy-audio-output-set-default", &[&id, name]).map(|_| ())
        }
        SystemAction::SetDefaultAudioSource { id, name } => {
            validate_argument(name, "audio source")?;
            let id = id.to_string();
            command("omarchy-audio-input-set-default", &[&id, name]).map(|_| ())
        }
        SystemAction::MediaPlayPause => media_action("PlayPause", &["-a", "play-pause"]),
        SystemAction::MediaPlay => media_action("Play", &["-a", "play"]),
        SystemAction::MediaPause => media_action("Pause", &["-a", "pause"]),
        SystemAction::MediaNext => media_action("Next", &["-a", "next"]),
        SystemAction::MediaPrevious => media_action("Previous", &["-a", "previous"]),
        SystemAction::SetWifiEnabled(enabled) => command(
            "nmcli",
            &["radio", "wifi", if *enabled { "on" } else { "off" }],
        )
        .map(|_| ()),
        SystemAction::SetBluetoothPower(powered) => command(
            "omarchy-bluetooth-power",
            &[if *powered { "on" } else { "off" }],
        )
        .map(|_| ()),
        SystemAction::ActivateNetwork(connection) => {
            validate_argument(connection, "network connection")?;
            command("nmcli", &["connection", "up", "id", connection]).map(|_| ())
        }
        SystemAction::SetBrightness { monitor, percent } => {
            validate_argument(monitor, "monitor")?;
            let value = format!("{}%", (*percent).clamp(1, 100));
            command(
                "omarchy-brightness-display",
                &["--no-osd", "--monitor", monitor, &value],
            )
            .map(|_| ())
        }
        SystemAction::ToggleDisplay { name, enabled } => {
            validate_argument(name, "monitor")?;
            let rule = if *enabled {
                format!("{name},preferred,auto,auto")
            } else {
                format!("{name},disable")
            };
            command("hyprctl", &["keyword", "monitor", &rule]).map(|_| ())
        }
        SystemAction::SetMonitorScale(scale) => {
            validate_argument(scale, "monitor scale")?;
            command("omarchy-hyprland-monitor-scaling", &[scale]).map(|_| ())
        }
        SystemAction::SetTextSize(size) => {
            if !(9..=20).contains(size) {
                return Err("invalid text size".to_string());
            }
            let value = size.to_string();
            command("omarchy-display-text-size", &[&value]).map(|_| ())
        }
        SystemAction::SetPowerProfile {
            profile,
            on_battery,
        } => {
            validate_argument(profile, "power profile")?;
            command(
                "omarchy-powerprofiles-set",
                &[if *on_battery { "battery" } else { "ac" }, profile],
            )
            .map(|_| ())
        }
        SystemAction::SetNetworkBand(band) => {
            if !matches!(band.as_str(), "auto" | "2.4" | "5" | "6") {
                return Err("invalid network band".to_string());
            }
            command("omarchy-network-band", &[band]).map(|_| ())
        }
        SystemAction::SetNightlight(enabled) => set_nightlight(*enabled),
        SystemAction::BluetoothDevice { action, address } => {
            validate_bluetooth_address(address)?;
            let command_name = match action {
                BluetoothDeviceAction::Pair => "pair",
                BluetoothDeviceAction::Connect => "connect",
                BluetoothDeviceAction::Disconnect => "disconnect",
                BluetoothDeviceAction::Forget => "forget",
            };
            command(
                "omarchy-bluetooth-device",
                &[command_name, address.as_str()],
            )
            .map(|_| ())
        }
        SystemAction::ConnectNetwork { ssid, device } => {
            validate_argument(ssid, "network SSID")?;
            validate_argument(device, "network device")?;
            command(
                "nmcli",
                &["device", "wifi", "connect", ssid, "ifname", device],
            )
            .map(|_| ())
        }
        SystemAction::DisconnectNetwork(device) => {
            validate_argument(device, "network device")?;
            command("nmcli", &["device", "disconnect", device]).map(|_| ())
        }
        SystemAction::ForgetNetwork(connection) => {
            validate_argument(connection, "network connection")?;
            command("nmcli", &["connection", "delete", "id", connection]).map(|_| ())
        }
        SystemAction::RescanWifi(device) => {
            validate_argument(device, "network device")?;
            command("nmcli", &["device", "wifi", "rescan", "ifname", device]).map(|_| ())
        }
    }
}

fn set_nightlight(enabled: bool) -> Result<(), String> {
    if !command_present("hyprsunset") {
        Command::new("setsid")
            .args(["uwsm-app", "--", "hyprsunset"])
            .spawn()
            .map_err(|error| format!("start hyprsunset: {error}"))?;
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let temperature = if enabled { "4000" } else { "6500" };
    command("hyprctl", &["hyprsunset", "temperature", temperature]).map(|_| ())
}

fn command_present(program: &str) -> bool {
    Command::new("which")
        .arg(program)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn media_action(method: &str, playerctl_args: &[&str]) -> Result<(), String> {
    match dbus::call_default_player(method) {
        Ok(()) => Ok(()),
        Err(dbus_error) => command("playerctl", playerctl_args).map(|_| ()).map_err(
            |fallback_error| {
                format!(
                    "MPRIS unavailable ({dbus_error}); playerctl fallback failed ({fallback_error})"
                )
            },
        ),
    }
}

pub fn to_value(snapshot: &SystemSnapshot) -> Value {
    serde_json::json!({
        "collectedAt": snapshot.collected_at,
        "hyprland": {
            "available": snapshot.hyprland.available,
            "activeWorkspace": snapshot.hyprland.active_workspace,
            "monitor": snapshot.hyprland.monitor,
            "activeWindow": snapshot.hyprland.active_window,
            "activeClass": snapshot.hyprland.active_class,
            "activeAddress": snapshot.hyprland.active_address,
            "workspaces": snapshot.hyprland.workspaces.iter().map(|workspace| serde_json::json!({
                "id": workspace.id,
                "name": workspace.name,
                "monitor": workspace.monitor,
                "windows": workspace.windows,
            })).collect::<Vec<_>>(),
            "monitors": snapshot.hyprland.monitors.iter().map(|monitor| serde_json::json!({
                "name": monitor.name,
                "description": monitor.description,
                "make": monitor.make,
                "model": monitor.model,
                "serial": monitor.serial,
                "width": monitor.width,
                "height": monitor.height,
                "refreshRate": monitor.refresh_rate,
                "x": monitor.x,
                "y": monitor.y,
                "scale": monitor.scale,
                "focused": monitor.focused,
                "enabled": monitor.enabled,
                "mirrorOf": monitor.mirror_of,
            })).collect::<Vec<_>>(),
            "error": snapshot.hyprland.error,
        },
        "audio": {
            "available": snapshot.audio.available,
            "outputPercent": snapshot.audio.output_percent,
            "outputMuted": snapshot.audio.output_muted,
            "inputPercent": snapshot.audio.input_percent,
            "inputMuted": snapshot.audio.input_muted,
            "sinks": snapshot.audio.sinks.iter().map(audio_node_value).collect::<Vec<_>>(),
            "sources": snapshot.audio.sources.iter().map(audio_node_value).collect::<Vec<_>>(),
            "streams": snapshot.audio.streams.iter().map(audio_node_value).collect::<Vec<_>>(),
            "error": snapshot.audio.error,
        },
        "network": {
            "available": snapshot.network.available,
            "wifiEnabled": snapshot.network.wifi_enabled,
            "device": snapshot.network.device,
            "kind": snapshot.network.kind,
            "connection": snapshot.network.connection,
            "ssid": snapshot.network.ssid,
            "signalPercent": snapshot.network.signal_percent,
            "frequencyMhz": snapshot.network.frequency_mhz,
            "details": {
                "iface": snapshot.network.details.iface,
                "ip": snapshot.network.details.ip,
                "prefix": snapshot.network.details.prefix,
                "gateway": snapshot.network.details.gateway,
                "rxBytes": snapshot.network.details.rx_bytes,
                "txBytes": snapshot.network.details.tx_bytes,
                "signalDbm": snapshot.network.details.signal_dbm,
                "frequencyMhz": snapshot.network.details.frequency_mhz,
                "bitrate": snapshot.network.details.bitrate,
                "routerPingMs": snapshot.network.details.router_ping_ms,
                "internetPingMs": snapshot.network.details.internet_ping_ms,
            },
            "band": {
                "current": snapshot.network.band.current,
                "selected": snapshot.network.band.selected,
                "available": snapshot.network.band.available,
            },
            "wifiNetworks": snapshot.network.wifi_networks.iter().map(|network| serde_json::json!({
                "ssid": network.ssid,
                "signalPercent": network.signal_percent,
                "frequencyMhz": network.frequency_mhz,
                "security": network.security,
                "connected": network.connected,
                "known": network.known,
                "device": network.device,
            })).collect::<Vec<_>>(),
            "error": snapshot.network.error,
        },
        "bluetooth": {
            "available": snapshot.bluetooth.available,
            "powered": snapshot.bluetooth.powered,
            "connectedDevices": snapshot.bluetooth.connected_devices,
            "devices": snapshot.bluetooth.devices.iter().map(|device| serde_json::json!({
                "address": device.address,
                "name": device.name,
                "connected": device.connected,
            })).collect::<Vec<_>>(),
            "error": snapshot.bluetooth.error,
        },
        "battery": {
            "available": snapshot.battery.available,
            "percentage": snapshot.battery.percentage,
            "charging": snapshot.battery.charging,
            "state": snapshot.battery.state,
            "rate": snapshot.battery.rate,
            "size": snapshot.battery.size,
            "time": snapshot.battery.time_remaining,
            "cycles": snapshot.battery.cycles,
            "threshold": snapshot.battery.threshold,
            "error": snapshot.battery.error,
        },
        "media": {
            "available": snapshot.media.available,
            "player": snapshot.media.player,
            "status": snapshot.media.status,
            "artist": snapshot.media.artist,
            "title": snapshot.media.title,
            "players": snapshot.media.players.iter().map(|player| serde_json::json!({
                "player": player.player,
                "busName": player.bus_name,
                "desktopEntry": player.desktop_entry,
                "status": player.status,
                "artist": player.artist,
                "title": player.title,
                "album": player.album,
                "artUrl": player.art_url,
                "canGoNext": player.can_go_next,
                "canGoPrevious": player.can_go_previous,
                "canPlay": player.can_play,
                "canPause": player.can_pause,
            })).collect::<Vec<_>>(),
            "error": snapshot.media.error,
        },
        "display": {
            "available": snapshot.display.available,
            "brightness": snapshot.display.brightness_percent,
            "brightnessAvailable": snapshot.display.brightness_available,
            "internalMonitor": snapshot.display.internal_monitor,
            "externalMonitor": snapshot.display.external_monitor,
            "focusedMonitor": snapshot.display.focused_monitor,
            "internalEnabled": snapshot.display.internal_enabled,
            "mirrorEnabled": snapshot.display.mirror_enabled,
            "scale": snapshot.display.monitor_scale,
            "textSize": snapshot.display.text_size,
            "displays": snapshot.display.displays.iter().map(|display| serde_json::json!({
                "name": display.name,
                "enabled": display.enabled,
                "focused": display.focused,
                "width": display.width,
                "height": display.height,
            })).collect::<Vec<_>>(),
            "error": snapshot.display.error,
        },
        "power": {
            "activeProfile": snapshot.power.active_profile,
            "profiles": snapshot.power.profiles.iter().map(|profile| serde_json::json!({
                "name": profile.name,
                "active": profile.active,
            })).collect::<Vec<_>>(),
            "error": snapshot.power.error,
        },
        "resources": {
            "cpuPercent": snapshot.resources.cpu_percent,
            "memoryUsed": snapshot.resources.memory_used,
            "memoryTotal": snapshot.resources.memory_total,
            "load": snapshot.resources.load,
            "error": snapshot.resources.error,
        },
        "nightlight": {
            "available": snapshot.nightlight.available,
            "temperature": snapshot.nightlight.temperature,
            "active": snapshot.nightlight.active,
            "error": snapshot.nightlight.error,
        },
    })
}

fn audio_node_value(node: &AudioNode) -> Value {
    serde_json::json!({
        "id": node.id,
        "name": node.name,
        "description": node.description,
        "application": node.application,
        "type": node.node_type,
        "volume": node.volume,
        "muted": node.muted,
        "default": node.is_default,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HyprlandState {
    pub available: bool,
    pub active_workspace: String,
    pub workspaces: Vec<WorkspaceState>,
    pub monitors: Vec<MonitorState>,
    pub active_window: String,
    pub active_class: String,
    pub active_address: String,
    pub monitor: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceState {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    pub windows: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonitorState {
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub width: i64,
    pub height: i64,
    pub refresh_rate: String,
    pub x: i64,
    pub y: i64,
    pub scale: String,
    pub focused: bool,
    pub enabled: bool,
    pub mirror_of: String,
}

impl HyprlandState {
    fn collect() -> Self {
        let monitors = match command_json("hyprctl", &["-j", "monitors"]) {
            Ok(value) => value,
            Err(error) => return Self::with_error(error),
        };
        let workspaces = command_json("hyprctl", &["-j", "workspaces"]).ok();
        let active_window = command_json("hyprctl", &["-j", "activewindow"]).ok();
        parse_hyprland(&monitors, workspaces.as_ref(), active_window.as_ref())
    }

    fn with_error(error: String) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }
}

pub fn parse_hyprland(
    monitors: &Value,
    workspaces: Option<&Value>,
    active_window: Option<&Value>,
) -> HyprlandState {
    let monitor_items = monitors.as_array().cloned().unwrap_or_default();
    let parsed_monitors = monitor_items
        .iter()
        .map(|monitor| MonitorState {
            name: value_string(monitor, "name"),
            description: value_string(monitor, "description"),
            make: value_string(monitor, "make"),
            model: value_string(monitor, "model"),
            serial: value_string(monitor, "serial"),
            width: value_i64(monitor, "width"),
            height: value_i64(monitor, "height"),
            refresh_rate: monitor
                .get("refreshRate")
                .and_then(Value::as_f64)
                .map(|rate| format!("{rate:.2}"))
                .unwrap_or_default(),
            x: value_i64(monitor, "x"),
            y: value_i64(monitor, "y"),
            scale: monitor
                .get("scale")
                .map(|scale| {
                    scale
                        .as_f64()
                        .map(|value| format!("{value:.2}"))
                        .unwrap_or_else(|| scale.as_str().unwrap_or_default().to_string())
                })
                .unwrap_or_default(),
            focused: monitor
                .get("focused")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            enabled: !monitor
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            mirror_of: value_string(monitor, "mirrorOf"),
        })
        .collect::<Vec<_>>();
    let monitor = monitor_items
        .iter()
        .find(|monitor| monitor.get("focused").and_then(Value::as_bool) == Some(true))
        .or_else(|| monitor_items.first())
        .cloned()
        .unwrap_or(Value::Null);
    let active_workspace = monitor
        .get("activeWorkspace")
        .and_then(|workspace| workspace.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            monitor
                .get("activeWorkspace")
                .and_then(|workspace| workspace.get("id"))
                .and_then(Value::as_i64)
                .map(|_| "")
        })
        .unwrap_or("")
        .to_string();

    let mut parsed_workspaces = workspaces
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(WorkspaceState {
                        id: item.get("id")?.as_i64()?,
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        monitor: item
                            .get("monitor")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        windows: item.get("windows").and_then(Value::as_i64).unwrap_or(0),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    parsed_workspaces.sort_by_key(|workspace| workspace.id);

    let active_window = active_window.cloned().unwrap_or(Value::Null);
    let active_title = active_window
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let active_class = active_window
        .get("class")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let active_address = active_window
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    HyprlandState {
        available: monitors.is_array(),
        active_workspace,
        workspaces: parsed_workspaces,
        monitors: parsed_monitors,
        active_window: active_title,
        active_class,
        active_address,
        monitor: monitor
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        error: None,
    }
}

fn value_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn value_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or_default()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioState {
    pub available: bool,
    pub output_percent: Option<u8>,
    pub output_muted: bool,
    pub input_percent: Option<u8>,
    pub input_muted: bool,
    pub sinks: Vec<AudioNode>,
    pub sources: Vec<AudioNode>,
    pub streams: Vec<AudioNode>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioEndpoint {
    pub percent: Option<u8>,
    pub muted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioNode {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub application: String,
    pub node_type: String,
    pub volume: Option<u8>,
    pub muted: bool,
    pub is_default: bool,
}

impl AudioState {
    fn collect() -> Self {
        let output = command("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).ok();
        let input = command("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"]).ok();
        let default_sink = command("wpctl", &["inspect", "@DEFAULT_AUDIO_SINK@"])
            .ok()
            .and_then(|raw| parse_wpctl_id(&raw));
        let default_source = command("wpctl", &["inspect", "@DEFAULT_AUDIO_SOURCE@"])
            .ok()
            .and_then(|raw| parse_wpctl_id(&raw));
        let mut inventory = command_json("pw-dump", &[])
            .ok()
            .map(|value| parse_pw_dump_audio(&value))
            .unwrap_or_default();
        for node in inventory
            .sinks
            .iter_mut()
            .chain(inventory.sources.iter_mut())
            .chain(inventory.streams.iter_mut())
        {
            let id = node.id.to_string();
            if let Ok(raw) = command("wpctl", &["get-volume", &id]) {
                let endpoint = parse_wpctl_volume(&raw);
                node.volume = endpoint.percent;
                node.muted = endpoint.muted;
            }
        }
        if let Some(id) = default_sink {
            if let Some(node) = inventory.sinks.iter_mut().find(|node| node.id == id) {
                node.is_default = true;
            }
        }
        if let Some(id) = default_source {
            if let Some(node) = inventory.sources.iter_mut().find(|node| node.id == id) {
                node.is_default = true;
            }
        }
        if output.is_none()
            && input.is_none()
            && inventory.sinks.is_empty()
            && inventory.sources.is_empty()
        {
            return Self {
                error: Some("wpctl unavailable".to_string()),
                ..Self::default()
            };
        }
        Self {
            available: output.is_some()
                || input.is_some()
                || !inventory.sinks.is_empty()
                || !inventory.sources.is_empty(),
            output_percent: output
                .as_deref()
                .map(parse_wpctl_volume)
                .and_then(|endpoint| endpoint.percent),
            output_muted: output
                .as_deref()
                .map(parse_wpctl_volume)
                .is_some_and(|endpoint| endpoint.muted),
            input_percent: input
                .as_deref()
                .map(parse_wpctl_volume)
                .and_then(|endpoint| endpoint.percent),
            input_muted: input
                .as_deref()
                .map(parse_wpctl_volume)
                .is_some_and(|endpoint| endpoint.muted),
            sinks: inventory.sinks,
            sources: inventory.sources,
            streams: inventory.streams,
            error: None,
        }
    }
}

pub fn parse_wpctl_volume(raw: &str) -> AudioEndpoint {
    let mut percent = None;
    for token in raw.split_whitespace() {
        if let Ok(value) = token.parse::<f32>() {
            percent = Some((value * 100.0).round().clamp(0.0, 100.0) as u8);
            break;
        }
    }
    AudioEndpoint {
        percent,
        muted: raw.split_whitespace().any(|token| token == "[MUTED]"),
    }
}

pub fn parse_wpctl_id(raw: &str) -> Option<u32> {
    raw.lines()
        .find_map(|line| line.trim().strip_prefix("id "))
        .and_then(|value| value.split(',').next())
        .and_then(|value| value.trim().parse::<u32>().ok())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioInventory {
    pub sinks: Vec<AudioNode>,
    pub sources: Vec<AudioNode>,
    pub streams: Vec<AudioNode>,
}

pub fn parse_pw_dump_audio(value: &Value) -> AudioInventory {
    let mut inventory = AudioInventory::default();
    for object in value.as_array().into_iter().flatten() {
        if object.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(info) = object.get("info") else {
            continue;
        };
        let props = info.get("props").unwrap_or(&Value::Null);
        let media_class = props
            .get("media.class")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let node_type = if media_class == "Audio/Sink" {
            "sink"
        } else if media_class.starts_with("Audio/Source") {
            "source"
        } else if media_class.contains("Stream") && media_class.contains("Audio") {
            "stream"
        } else {
            continue;
        };
        let Some(id) = object
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
        else {
            continue;
        };
        let name = value_string(props, "node.name");
        let description = props
            .get("node.description")
            .and_then(Value::as_str)
            .or_else(|| props.get("node.nick").and_then(Value::as_str))
            .or_else(|| props.get("media.name").and_then(Value::as_str))
            .unwrap_or(&name)
            .to_string();
        let application = props
            .get("application.name")
            .and_then(Value::as_str)
            .or_else(|| props.get("media.name").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();
        let (volume, muted) = parse_pw_dump_volume(info.get("params"));
        let node = AudioNode {
            id,
            name,
            description,
            application,
            node_type: node_type.to_string(),
            volume,
            muted,
            is_default: false,
        };
        match node_type {
            "sink" => inventory.sinks.push(node),
            "source" => inventory.sources.push(node),
            _ => inventory.streams.push(node),
        }
    }
    inventory
        .sinks
        .sort_by_key(|node| (!node.is_default, node.description.to_lowercase()));
    inventory
        .sources
        .sort_by_key(|node| (!node.is_default, node.description.to_lowercase()));
    inventory
        .streams
        .sort_by_key(|node| node.description.to_lowercase());
    inventory
}

fn parse_pw_dump_volume(params: Option<&Value>) -> (Option<u8>, bool) {
    let Some(params) = params.and_then(Value::as_array) else {
        return (None, false);
    };
    for parameter in params {
        let Some(object) = parameter.as_object() else {
            continue;
        };
        let muted = object.get("mute").and_then(Value::as_bool).unwrap_or(false)
            || object
                .get("softMute")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let volume = object
            .get("channelVolumes")
            .and_then(Value::as_array)
            .and_then(|values| {
                let values = values.iter().filter_map(Value::as_f64).collect::<Vec<_>>();
                (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
            })
            .or_else(|| object.get("volume").and_then(Value::as_f64));
        if volume.is_some() || muted {
            return (
                volume.map(|value| (value * 100.0).round().clamp(0.0, 100.0) as u8),
                muted,
            );
        }
    }
    (None, false)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkState {
    pub available: bool,
    pub wifi_enabled: Option<bool>,
    pub device: String,
    pub kind: String,
    pub connection: String,
    pub ssid: String,
    pub signal_percent: Option<u8>,
    pub frequency_mhz: String,
    pub details: NetworkDetails,
    pub wifi_networks: Vec<WifiNetwork>,
    pub band: NetworkBand,
    pub error: Option<String>,
}

impl NetworkState {
    fn collect() -> Self {
        let status = command("omarchy-network-status", &[])
            .ok()
            .map(|raw| parse_network_status(&raw));
        let mut network = status.unwrap_or_default();
        if let Ok(raw) = command(
            "nmcli",
            &[
                "-t",
                "--escape",
                "no",
                "-f",
                "DEVICE,TYPE,STATE,CONNECTION",
                "device",
                "status",
            ],
        ) {
            let nmcli = parse_nmcli_device_status(&raw);
            if !nmcli.device.is_empty() {
                network.device = nmcli.device;
                network.kind = nmcli.kind;
                if !nmcli.connection.is_empty() {
                    network.connection = nmcli.connection;
                }
                network.available = nmcli.available || network.available;
            }
        }
        network.wifi_enabled = command("nmcli", &["radio", "wifi"])
            .ok()
            .and_then(|raw| parse_nmcli_radio_wifi(&raw));
        if let Ok(raw) = command("omarchy-network-band", &[]) {
            network.band = parse_network_band(&raw);
        }
        if let Ok(raw) = command("omarchy-network-status", &["--verbose"]) {
            network.details = parse_network_verbose(&raw);
        }
        if let Ok(raw) = command(
            "nmcli",
            &[
                "-t",
                "--escape",
                "no",
                "-f",
                "IN-USE,SSID,SIGNAL,FREQ,SECURITY,DEVICE",
                "device",
                "wifi",
                "list",
                "--rescan",
                "no",
            ],
        ) {
            network.wifi_networks = parse_nmcli_wifi_list(&raw);
        }
        network
    }
}

pub fn parse_nmcli_radio_wifi(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "enabled" | "yes" | "on" | "true" => Some(true),
        "disabled" | "no" | "off" | "false" => Some(false),
        _ => None,
    }
}

pub fn parse_nmcli_device_status(raw: &str) -> NetworkState {
    let mut fallback = None;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() < 4 {
            continue;
        }
        let candidate = NetworkState {
            available: true,
            device: fields[0].to_string(),
            kind: fields[1].to_string(),
            connection: fields[3..].join(":"),
            error: None,
            ..Default::default()
        };
        if fields[2] == "connected" {
            return candidate;
        }
        fallback = Some(candidate);
    }
    fallback.unwrap_or_default()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkDetails {
    pub iface: String,
    pub ip: String,
    pub prefix: String,
    pub gateway: String,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
    pub signal_dbm: String,
    pub frequency_mhz: String,
    pub bitrate: String,
    pub router_ping_ms: String,
    pub internet_ping_ms: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal_percent: i32,
    pub frequency_mhz: String,
    pub security: String,
    pub connected: bool,
    pub known: bool,
    pub device: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkBand {
    pub current: String,
    pub selected: String,
    pub available: Vec<String>,
}

pub fn parse_network_status(raw: &str) -> NetworkState {
    let line = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let fields = line.split('\t').collect::<Vec<_>>();
    let kind = fields
        .first()
        .copied()
        .unwrap_or("disconnected")
        .to_string();
    let connection = fields.get(1).copied().unwrap_or_default().to_string();
    let signal_percent = fields
        .get(2)
        .and_then(|value| value.parse::<u8>().ok())
        .map(|value| value.min(100));
    let frequency_mhz = fields.get(3).copied().unwrap_or_default().to_string();
    NetworkState {
        available: kind != "disconnected",
        kind,
        connection: connection.clone(),
        ssid: connection,
        signal_percent,
        frequency_mhz,
        ..Default::default()
    }
}

pub fn parse_network_verbose(raw: &str) -> NetworkDetails {
    let mut details = NetworkDetails::default();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let value = value.trim().to_string();
        match key {
            "iface" => details.iface = value,
            "ip" => details.ip = value,
            "prefix" => details.prefix = value,
            "gateway" => details.gateway = value,
            "rx_bytes" => details.rx_bytes = value.parse().ok(),
            "tx_bytes" => details.tx_bytes = value.parse().ok(),
            "signal_dbm" => details.signal_dbm = value,
            "freq" => details.frequency_mhz = value,
            "bitrate" => details.bitrate = value,
            "router_ping_ms" => details.router_ping_ms = value,
            "internet_ping_ms" => details.internet_ping_ms = value,
            _ => {}
        }
    }
    details
}

pub fn parse_network_band(raw: &str) -> NetworkBand {
    let mut band = NetworkBand::default();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        match key {
            "band" => band.current = value.trim().to_string(),
            "selected" => band.selected = value.trim().to_string(),
            "available" => band.available = value.split_whitespace().map(str::to_string).collect(),
            _ => {}
        }
    }
    if band.selected.is_empty() {
        band.selected = "auto".to_string();
    }
    band
}

pub fn parse_nmcli_wifi_list(raw: &str) -> Vec<WifiNetwork> {
    let mut networks = raw
        .lines()
        .filter_map(|line| {
            let tab_fields = line.split('\t').collect::<Vec<_>>();
            let fields = if tab_fields.len() >= 6 {
                tab_fields
            } else {
                line.split(':').collect::<Vec<_>>()
            };
            let (ssid, signal_index, frequency, security, device) =
                if fields.len() >= 6 && fields[2].parse::<i32>().is_ok() {
                    (
                        fields[1].to_string(),
                        2,
                        fields[3].to_string(),
                        fields[4].to_string(),
                        fields[5..].join(":"),
                    )
                } else {
                    let signal_index = (1..fields.len().saturating_sub(2)).find(|index| {
                        fields[*index].parse::<i32>().is_ok()
                            && fields
                                .get(*index + 1)
                                .and_then(|value| value.parse::<f64>().ok())
                                .is_some()
                    })?;
                    (
                        fields[1..signal_index].join(":"),
                        signal_index,
                        fields[signal_index + 1].to_string(),
                        fields
                            .get(signal_index + 2)
                            .copied()
                            .unwrap_or_default()
                            .to_string(),
                        fields[signal_index + 3..].join(":"),
                    )
                };
            Some(WifiNetwork {
                connected: fields[0] == "*",
                ssid,
                signal_percent: fields[signal_index].parse().unwrap_or(-1).clamp(-1, 100),
                frequency_mhz: frequency,
                security,
                device,
                ..Default::default()
            })
        })
        .collect::<Vec<_>>();
    networks.sort_by(|left, right| {
        right
            .connected
            .cmp(&left.connected)
            .then_with(|| right.signal_percent.cmp(&left.signal_percent))
            .then_with(|| left.ssid.cmp(&right.ssid))
    });
    networks
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothState {
    pub available: bool,
    pub powered: bool,
    pub connected_devices: usize,
    pub devices: Vec<BluetoothDevice>,
    pub error: Option<String>,
}

impl BluetoothState {
    fn collect() -> Self {
        let show = match command("bluetoothctl", &["show"]) {
            Ok(raw) => raw,
            Err(error) => {
                return Self {
                    error: Some(error),
                    ..Self::default()
                };
            }
        };
        let connected_raw = command("bluetoothctl", &["devices", "Connected"]).unwrap_or_default();
        let devices_raw = command("bluetoothctl", &["devices"]).unwrap_or_default();
        let devices = parse_bluetooth_devices(&devices_raw, &connected_raw);
        let mut state = parse_bluetooth_show(&show, devices.len());
        state.devices = devices;
        state
    }
}

pub fn parse_bluetooth_show(raw: &str, connected_devices: usize) -> BluetoothState {
    let powered = raw.lines().any(|line| {
        line.trim_start()
            .strip_prefix("Powered:")
            .is_some_and(|value| value.trim() == "yes")
    });
    BluetoothState {
        available: raw
            .lines()
            .any(|line| line.trim_start().starts_with("Controller")),
        powered,
        connected_devices,
        devices: Vec::new(),
        error: None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothDevice {
    pub address: String,
    pub name: String,
    pub connected: bool,
}

pub fn parse_bluetooth_devices(all_raw: &str, connected_raw: &str) -> Vec<BluetoothDevice> {
    let connected = connected_raw
        .lines()
        .filter_map(parse_bluetooth_device_line)
        .map(|device| device.address)
        .collect::<BTreeSet<_>>();
    let mut devices = all_raw
        .lines()
        .filter_map(parse_bluetooth_device_line)
        .map(|mut device| {
            device.connected = connected.contains(&device.address);
            device
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| {
        right
            .connected
            .cmp(&left.connected)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.address.cmp(&right.address))
    });
    devices
}

fn parse_bluetooth_device_line(line: &str) -> Option<BluetoothDevice> {
    let mut fields = line.splitn(3, ' ');
    if fields.next()? != "Device" {
        return None;
    }
    let address = fields.next()?.trim();
    if address.is_empty() {
        return None;
    }
    Some(BluetoothDevice {
        address: address.to_string(),
        name: fields.next().unwrap_or_default().trim().to_string(),
        connected: false,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatteryState {
    pub available: bool,
    pub percentage: Option<u8>,
    pub charging: bool,
    pub state: String,
    pub rate: String,
    pub size: String,
    pub time_remaining: String,
    pub cycles: String,
    pub threshold: String,
    pub error: Option<String>,
}

impl BatteryState {
    fn collect() -> Self {
        if let Ok(raw) = command("omarchy-battery-status", &["--shell"]) {
            let parsed = parse_battery_shell(&raw);
            if parsed.available {
                return parsed;
            }
        }
        let raw = match command(
            "upower",
            &["-i", "/org/freedesktop/UPower/devices/DisplayDevice"],
        ) {
            Ok(raw) => raw,
            Err(error) => {
                return Self {
                    error: Some(error),
                    ..Self::default()
                };
            }
        };
        parse_upower_display(&raw)
    }
}

pub fn parse_upower_display(raw: &str) -> BatteryState {
    let mut percentage = None;
    let mut state = String::new();
    for line in raw.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        match key.trim() {
            "percentage" => {
                percentage = value
                    .trim()
                    .trim_end_matches('%')
                    .parse::<u8>()
                    .ok()
                    .map(|value| value.min(100));
            }
            "state" => state = value.trim().to_string(),
            _ => {}
        }
    }
    BatteryState {
        available: percentage.is_some() || !state.is_empty(),
        charging: matches!(state.as_str(), "charging" | "fully-charged"),
        percentage,
        state,
        error: None,
        ..Default::default()
    }
}

pub fn parse_battery_shell(raw: &str) -> BatteryState {
    let mut state = BatteryState::default();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let value = value.trim().to_string();
        match key {
            "percentage" => {
                state.percentage = value
                    .trim_end_matches('%')
                    .parse::<u8>()
                    .ok()
                    .map(|value| value.min(100));
            }
            "state" => state.state = value,
            "rate" => state.rate = value,
            "size" => state.size = value,
            "time" => state.time_remaining = value,
            "cycles" => state.cycles = value,
            "threshold" => state.threshold = value,
            _ => {}
        }
    }
    state.available = state.percentage.is_some() || !state.state.is_empty();
    state.charging = matches!(state.state.as_str(), "charging" | "fully-charged");
    state
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaPlayerState {
    pub player: String,
    pub bus_name: String,
    pub desktop_entry: String,
    pub status: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub art_url: String,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_play: bool,
    pub can_pause: bool,
}

impl MediaPlayerState {
    pub fn key(&self) -> &str {
        if self.bus_name.is_empty() {
            &self.player
        } else {
            &self.bus_name
        }
    }

    pub fn has_metadata(&self) -> bool {
        !self.title.is_empty() || !self.artist.is_empty() || !self.album.is_empty()
    }

    pub fn can_toggle_playing(&self) -> bool {
        self.can_play || self.can_pause
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaState {
    pub available: bool,
    pub player: String,
    pub status: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub art_url: String,
    pub players: Vec<MediaPlayerState>,
    pub error: Option<String>,
}

impl MediaState {
    fn collect() -> Self {
        match dbus::list_mpris_players() {
            Ok(players) if !players.is_empty() => return media_state_from_mpris(players),
            Ok(_) | Err(_) => {}
        }
        match command(
            "playerctl",
            &[
                "-a",
                "metadata",
                "--format",
                "{{playerName}}\t{{status}}\t{{artist}}\t{{title}}\t{{album}}\t{{mpris:artUrl}}",
            ],
        ) {
            Ok(raw) => parse_playerctl_metadata(&raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }
}

fn media_state_from_mpris(players: Vec<dbus::MprisPlayer>) -> MediaState {
    let players = players
        .into_iter()
        .map(|player| MediaPlayerState {
            player: if player.identity.is_empty() {
                player.bus_name.clone()
            } else {
                player.identity
            },
            bus_name: player.bus_name,
            desktop_entry: player.desktop_entry,
            status: player.status,
            artist: player.artist,
            title: player.title,
            album: player.album,
            art_url: player.art_url,
            can_go_next: player.can_go_next,
            can_go_previous: player.can_go_previous,
            can_play: player.can_play,
            can_pause: player.can_pause,
        })
        .collect::<Vec<_>>();
    let Some(player) = players
        .iter()
        .find(|player| player.status.eq_ignore_ascii_case("playing"))
        .or_else(|| players.iter().find(|player| player.has_metadata()))
        .or_else(|| players.first())
    else {
        return MediaState::default();
    };
    MediaState {
        available: true,
        player: player.player.clone(),
        status: player.status.clone(),
        artist: player.artist.clone(),
        title: player.title.clone(),
        album: player.album.clone(),
        art_url: player.art_url.clone(),
        players,
        error: None,
    }
}

pub fn parse_playerctl_metadata(raw: &str) -> MediaState {
    let players = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            MediaPlayerState {
                player: fields.first().copied().unwrap_or_default().to_string(),
                bus_name: String::new(),
                desktop_entry: String::new(),
                status: fields.get(1).copied().unwrap_or_default().to_string(),
                artist: fields.get(2).copied().unwrap_or_default().to_string(),
                title: fields.get(3).copied().unwrap_or_default().to_string(),
                album: fields.get(4).copied().unwrap_or_default().to_string(),
                art_url: fields.get(5).copied().unwrap_or_default().to_string(),
                can_go_next: true,
                can_go_previous: true,
                can_play: true,
                can_pause: true,
            }
        })
        .filter(|player| !player.player.is_empty())
        .collect::<Vec<_>>();
    let Some(player) = players
        .iter()
        .find(|player| player.status.eq_ignore_ascii_case("playing"))
        .or_else(|| players.first())
    else {
        return MediaState::default();
    };
    MediaState {
        available: true,
        player: player.player.clone(),
        status: player.status.clone(),
        artist: player.artist.clone(),
        title: player.title.clone(),
        album: player.album.clone(),
        art_url: player.art_url.clone(),
        players,
        error: None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayState {
    pub available: bool,
    pub brightness_percent: Option<u8>,
    pub brightness_available: bool,
    pub internal_monitor: String,
    pub external_monitor: String,
    pub focused_monitor: String,
    pub internal_enabled: bool,
    pub mirror_enabled: bool,
    pub monitor_scale: String,
    pub text_size: Option<u8>,
    pub displays: Vec<DisplayInfo>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayInfo {
    pub name: String,
    pub enabled: bool,
    pub focused: bool,
    pub width: i64,
    pub height: i64,
}

impl DisplayState {
    fn collect() -> Self {
        let raw = match command("omarchy-monitor-state", &[]) {
            Ok(raw) => raw,
            Err(error) => {
                return Self {
                    error: Some(error),
                    ..Self::default()
                };
            }
        };
        let mut state = parse_monitor_state(&raw);
        state.text_size = command("omarchy-display-text-size", &[])
            .ok()
            .and_then(|raw| parse_text_size(&raw));
        state
    }
}

pub fn parse_monitor_state(raw: &str) -> DisplayState {
    let lines = raw.split('\n').collect::<Vec<_>>();
    let brightness = lines
        .first()
        .and_then(|value| value.trim().parse::<u8>().ok())
        .map(|value| value.min(100));
    let displays = lines
        .get(7)
        .and_then(|value| serde_json::from_str::<Value>(value.trim()).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|display| {
            Some(DisplayInfo {
                name: display.get("name")?.as_str()?.to_string(),
                enabled: display
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                focused: display
                    .get("focused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                width: display
                    .get("width")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                height: display
                    .get("height")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let external_monitor = lines.get(2).copied().unwrap_or_default().trim().to_string();
    DisplayState {
        available: !displays.is_empty(),
        brightness_percent: brightness,
        brightness_available: brightness.is_some(),
        internal_monitor: lines.get(1).copied().unwrap_or_default().trim().to_string(),
        external_monitor: external_monitor.clone(),
        focused_monitor: lines.get(5).copied().unwrap_or_default().trim().to_string(),
        internal_enabled: !lines.get(3).copied().unwrap_or_default().trim().is_empty(),
        mirror_enabled: !external_monitor.is_empty()
            && lines.get(4).copied().unwrap_or_default().trim() == external_monitor,
        monitor_scale: lines.get(6).copied().unwrap_or_default().trim().to_string(),
        text_size: None,
        displays,
        error: None,
    }
}

pub fn parse_text_size(raw: &str) -> Option<u8> {
    raw.lines()
        .find_map(|line| line.trim().strip_prefix("text size:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|value| (9..=20).contains(value))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PowerState {
    pub profiles: Vec<PowerProfile>,
    pub active_profile: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PowerProfile {
    pub name: String,
    pub active: bool,
}

impl PowerState {
    fn collect() -> Self {
        match command("omarchy-powerprofiles-list", &["--active-state"]) {
            Ok(raw) => parse_power_profiles(&raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }
}

pub fn parse_power_profiles(raw: &str) -> PowerState {
    let profiles = raw
        .lines()
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            let name = fields.first()?.trim();
            if name.is_empty() {
                return None;
            }
            Some(PowerProfile {
                name: name.to_string(),
                active: fields.get(1).is_some_and(|value| value.trim() == "1"),
            })
        })
        .collect::<Vec<_>>();
    let active_profile = profiles
        .iter()
        .find(|profile| profile.active)
        .map(|profile| profile.name.clone())
        .unwrap_or_default();
    PowerState {
        profiles,
        active_profile,
        error: None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceState {
    pub available: bool,
    pub cpu_percent: Option<u8>,
    pub memory_used: String,
    pub memory_total: String,
    pub load: String,
    pub error: Option<String>,
}

impl ResourceState {
    fn collect() -> Self {
        match command("omarchy-system-stats", &[]) {
            Ok(raw) => parse_system_stats(&raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }
}

pub fn parse_system_stats(raw: &str) -> ResourceState {
    let mut state = ResourceState::default();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let value = value.trim();
        match key {
            "cpu" => {
                state.cpu_percent = value
                    .trim_end_matches('%')
                    .parse::<u8>()
                    .ok()
                    .map(|value| value.min(100));
            }
            "memory" => {
                let fields = value.split_once(" / ");
                state.memory_used = fields
                    .map(|(used, _)| used.to_string())
                    .unwrap_or_else(|| value.to_string());
                state.memory_total = fields
                    .map(|(_, total)| total.to_string())
                    .unwrap_or_default();
            }
            "load" => state.load = value.to_string(),
            _ => {}
        }
    }
    state.available =
        state.cpu_percent.is_some() || !state.memory_used.is_empty() || !state.load.is_empty();
    state
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NightlightState {
    pub available: bool,
    pub temperature: Option<u16>,
    pub active: bool,
    pub error: Option<String>,
}

impl NightlightState {
    fn collect() -> Self {
        match command("hyprctl", &["hyprsunset", "temperature"]) {
            Ok(raw) => parse_nightlight_temperature(&raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }
}

pub fn parse_nightlight_temperature(raw: &str) -> NightlightState {
    let temperature = raw
        .lines()
        .flat_map(|line| line.split_whitespace())
        .find_map(|value| value.parse::<u16>().ok());
    NightlightState {
        available: temperature.is_some(),
        active: temperature.is_some_and(|value| value < 6000),
        temperature,
        error: None,
    }
}

fn command(program: &str, args: &[&str]) -> Result<String, String> {
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

fn validate_argument(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(format!("invalid {label}"));
    }
    Ok(())
}

fn validate_bluetooth_address(address: &str) -> Result<(), String> {
    let valid = address.len() == 17
        && address.as_bytes().iter().enumerate().all(|(index, value)| {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                *value == b':'
            } else {
                value.is_ascii_hexdigit()
            }
        });
    if valid {
        Ok(())
    } else {
        Err("invalid Bluetooth address".to_string())
    }
}

fn command_json(program: &str, args: &[&str]) -> Result<Value, String> {
    let raw = command(program, args)?;
    serde_json::from_str(&raw).map_err(|error| format!("{program} returned invalid JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        SystemAction, parse_battery_shell, parse_bluetooth_devices, parse_bluetooth_show,
        parse_hyprland, parse_monitor_state, parse_network_band, parse_network_status,
        parse_network_verbose, parse_nightlight_temperature, parse_nmcli_device_status,
        parse_nmcli_radio_wifi, parse_nmcli_wifi_list, parse_playerctl_metadata,
        parse_power_profiles, parse_pw_dump_audio, parse_system_stats, parse_text_size,
        parse_upower_display, parse_wpctl_id, parse_wpctl_volume, run_action,
    };

    #[test]
    fn parses_hyprland_state_from_json_boundaries() {
        let monitors = json!([{
            "name": "HDMI-A-1",
            "activeWorkspace": {"id": 2, "name": "2"}
        }]);
        let workspaces = json!([
            {"id": 2, "name": "2", "monitor": "HDMI-A-1", "windows": 3},
            {"id": 1, "name": "1", "monitor": "HDMI-A-1", "windows": 1}
        ]);
        let active = json!({"title": "Terminal", "class": "foot", "address": "0x123"});
        let parsed = parse_hyprland(&monitors, Some(&workspaces), Some(&active));
        assert!(parsed.available);
        assert_eq!(parsed.active_workspace, "2");
        assert_eq!(parsed.workspaces[0].id, 1);
        assert_eq!(parsed.active_window, "Terminal");
        assert_eq!(parsed.active_address, "0x123");
        assert_eq!(parsed.monitor, "HDMI-A-1");
    }

    #[test]
    fn parses_wpctl_percent_and_mute_marker() {
        assert_eq!(
            parse_wpctl_volume("Volume: 0.30 [MUTED]\n"),
            super::AudioEndpoint {
                percent: Some(30),
                muted: true,
            }
        );
    }

    #[test]
    fn parses_pipewire_audio_inventory_and_authoritative_ids() {
        let dump = json!([
            {
                "id": 43,
                "type": "PipeWire:Interface:Node",
                "info": {
                    "props": {
                        "media.class": "Audio/Sink",
                        "node.name": "alsa_output.usb",
                        "node.description": "USB Headphones",
                        "node.nick": "USB",
                    },
                    "params": [{
                        "volume": 1.0,
                        "mute": false,
                        "channelVolumes": [0.3, 0.3]
                    }]
                }
            },
            {
                "id": 77,
                "type": "PipeWire:Interface:Node",
                "info": {
                    "props": {
                        "media.class": "Stream/Output/Audio",
                        "node.name": "spotify",
                        "node.description": "Spotify",
                        "application.name": "Spotify"
                    },
                    "params": [{
                        "volume": 1.0,
                        "mute": true
                    }]
                }
            },
            {
                "id": 99,
                "type": "PipeWire:Interface:Port",
                "info": {}
            }
        ]);
        let parsed = parse_pw_dump_audio(&dump);
        assert_eq!(parsed.sinks.len(), 1);
        assert_eq!(parsed.sinks[0].id, 43);
        assert_eq!(parsed.sinks[0].volume, Some(30));
        assert_eq!(parsed.sinks[0].description, "USB Headphones");
        assert_eq!(parsed.streams[0].application, "Spotify");
        assert!(parsed.streams[0].muted);
        assert_eq!(
            parse_wpctl_id("id 43, type PipeWire:Interface:Node\\n"),
            Some(43)
        );
        assert_eq!(parse_wpctl_id("not an inspect response"), None);
    }

    #[test]
    fn selects_connected_network_device() {
        let parsed = parse_nmcli_device_status(
            "enp1s0:ethernet:disconnected:--\nwlan0:wifi:connected:Home WiFi\n",
        );
        assert_eq!(parsed.device, "wlan0");
        assert_eq!(parsed.connection, "Home WiFi");
    }

    #[test]
    fn parses_reference_network_status_and_verbose_details() {
        let parsed = parse_network_status("wifi\tSTARLINK\t56\t5765.0\n");
        assert!(parsed.available);
        assert_eq!(parsed.kind, "wifi");
        assert_eq!(parsed.ssid, "STARLINK");
        assert_eq!(parsed.signal_percent, Some(56));
        assert_eq!(parsed.frequency_mhz, "5765.0");

        let details = parse_network_verbose(
            "iface\twlp6s0\nip\t192.168.1.219\nrx_bytes\t123\nsignal_dbm\t-64\ninternet_ping_ms\t17.2\n",
        );
        assert_eq!(details.iface, "wlp6s0");
        assert_eq!(details.rx_bytes, Some(123));
        assert_eq!(details.signal_dbm, "-64");
        assert_eq!(details.internet_ping_ms, "17.2");
    }

    #[test]
    fn sorts_wifi_rows_with_connected_network_first() {
        let rows = parse_nmcli_wifi_list(
            "*\tHome\t42\t2412\tWPA2\twlan0\n \tGuest\t88\t5180\tWPA2\twlan0\n \tOpen\t88\t2412\t\twlan0\n",
        );
        assert_eq!(rows[0].ssid, "Home");
        assert!(rows[0].connected);
        assert_eq!(rows[1].ssid, "Guest");
        assert_eq!(rows[2].ssid, "Open");
    }

    #[test]
    fn parses_network_band_state() {
        let band = parse_network_band("band\t5\navailable\t2.4 5\nselected\tauto\n");
        assert_eq!(band.current, "5");
        assert_eq!(band.available, vec!["2.4", "5"]);
        assert_eq!(band.selected, "auto");
    }

    #[test]
    fn parses_network_manager_wifi_radio_state() {
        assert_eq!(parse_nmcli_radio_wifi("enabled\n"), Some(true));
        assert_eq!(parse_nmcli_radio_wifi("disabled\n"), Some(false));
        assert_eq!(parse_nmcli_radio_wifi("unknown\n"), None);
    }

    #[test]
    fn parses_bluetooth_power_and_connections() {
        let parsed = parse_bluetooth_show(
            "Controller AA:BB:CC:DD:EE:FF host [default]\n\tPowered: yes\n",
            2,
        );
        assert!(parsed.available);
        assert!(parsed.powered);
        assert_eq!(parsed.connected_devices, 2);
    }

    #[test]
    fn parses_and_orders_bluetooth_devices() {
        let devices = parse_bluetooth_devices(
            "Device AA:BB:CC:DD:EE:FF Keyboard\nDevice 11:22:33:44:55:66 Headphones\n",
            "Device 11:22:33:44:55:66 Headphones\n",
        );
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "Headphones");
        assert!(devices[0].connected);
        assert!(!devices[1].connected);
    }

    #[test]
    fn parses_upower_display_state() {
        let parsed = parse_upower_display("state: charging\npercentage: 87%\n");
        assert!(parsed.available);
        assert!(parsed.charging);
        assert_eq!(parsed.percentage, Some(87));
    }

    #[test]
    fn parses_battery_shell_state_and_auxiliary_fields() {
        let parsed = parse_battery_shell(
            "percentage\t87%\nstate\tcharging\nrate\t18.4W\nsize\t54Wh\ntime\t1h 20m\ncycles\t42\nthreshold\t40-80%\n",
        );
        assert!(parsed.available);
        assert!(parsed.charging);
        assert_eq!(parsed.percentage, Some(87));
        assert_eq!(parsed.rate, "18.4W");
        assert_eq!(parsed.threshold, "40-80%");
    }

    #[test]
    fn parses_first_media_player() {
        let parsed = parse_playerctl_metadata(
            "Firefox\tPaused\tOld Artist\tOld Title\nVLC\tPlaying\tArtist\tTitle\tAlbum\tfile:///art.png\n",
        );
        assert!(parsed.available);
        assert_eq!(parsed.player, "VLC");
        assert_eq!(parsed.status, "Playing");
        assert_eq!(parsed.title, "Title");
        assert_eq!(parsed.album, "Album");
        assert_eq!(parsed.players.len(), 2);
    }

    #[test]
    fn parses_monitor_state_and_text_size() {
        let parsed = parse_monitor_state(
            "\n\nInternal\n\n\nHDMI-A-1\n1.25\n[{\"name\":\"HDMI-A-1\",\"enabled\":true,\"focused\":true,\"width\":3440,\"height\":1440}]\n",
        );
        assert!(parsed.available);
        assert_eq!(parsed.focused_monitor, "HDMI-A-1");
        assert_eq!(parsed.monitor_scale, "1.25");
        assert_eq!(parsed.displays[0].width, 3440);
        assert_eq!(
            parse_text_size("text size: 16 px\ngtk text-scaling-factor: 1.2\n"),
            Some(16)
        );
    }

    #[test]
    fn parses_power_profiles_resources_and_nightlight() {
        let power = parse_power_profiles("power-saver\t0\nbalanced\t1\nperformance\t0\n");
        assert_eq!(power.active_profile, "balanced");
        let resources = parse_system_stats("cpu\t13%\nmemory\t11.3GB / 31GB\nload\t0.42\n");
        assert_eq!(resources.cpu_percent, Some(13));
        assert_eq!(resources.memory_total, "31GB");
        assert_eq!(resources.load, "0.42");
        let nightlight = parse_nightlight_temperature("temperature: 4000\n");
        assert!(nightlight.active);
        assert_eq!(nightlight.temperature, Some(4000));
    }

    #[test]
    fn rejects_control_characters_before_shell_action() {
        let result = run_action(&SystemAction::FocusWorkspace(String::from("1\nquit")));
        assert_eq!(result, Err("invalid workspace".to_string()));
    }

    #[test]
    fn rejects_invalid_control_values_before_desktop_actions() {
        assert_eq!(
            run_action(&SystemAction::SetBrightness {
                monitor: "HDMI-A-1\n".to_string(),
                percent: 50,
            }),
            Err("invalid monitor".to_string())
        );
        assert_eq!(
            run_action(&SystemAction::SetTextSize(21)),
            Err("invalid text size".to_string())
        );
        assert_eq!(
            run_action(&SystemAction::SetNetworkBand("10".to_string())),
            Err("invalid network band".to_string())
        );
        assert_eq!(
            run_action(&SystemAction::BluetoothDevice {
                action: super::BluetoothDeviceAction::Connect,
                address: "not-an-address".to_string(),
            }),
            Err("invalid Bluetooth address".to_string())
        );
    }
}
