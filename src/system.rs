//! Read-only adapters for the services the Omarchy shell exposes in its bar.
//!
//! The reference shell gets these values from Quickshell integrations.  The
//! GPUI port keeps the first adapter layer deliberately small and explicit:
//! each adapter owns the command it talks to, parses a stable text/JSON
//! boundary, and records an error instead of turning an unavailable service
//! into a fabricated value.  The renderer can therefore keep running while a
//! service is missing, and the same parsers can be exercised with fixtures.

use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemSnapshot {
    pub hyprland: HyprlandState,
    pub audio: AudioState,
    pub network: NetworkState,
    pub bluetooth: BluetoothState,
    pub battery: BatteryState,
    pub media: MediaState,
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
            collected_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemAction {
    FocusWorkspace(String),
    ToggleOutputMute,
    ToggleInputMute,
    SetOutputVolume(u8),
    MediaPlayPause,
    MediaNext,
    MediaPrevious,
    SetBluetoothPower(bool),
    ActivateNetwork(String),
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
        SystemAction::MediaPlayPause => command("playerctl", &["-a", "play-pause"]).map(|_| ()),
        SystemAction::MediaNext => command("playerctl", &["-a", "next"]).map(|_| ()),
        SystemAction::MediaPrevious => command("playerctl", &["-a", "previous"]).map(|_| ()),
        SystemAction::SetBluetoothPower(powered) => command(
            "bluetoothctl",
            &["power", if *powered { "on" } else { "off" }],
        )
        .map(|_| ()),
        SystemAction::ActivateNetwork(connection) => {
            validate_argument(connection, "network connection")?;
            command("nmcli", &["connection", "up", "id", connection]).map(|_| ())
        }
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
            "workspaces": snapshot.hyprland.workspaces.iter().map(|workspace| serde_json::json!({
                "id": workspace.id,
                "name": workspace.name,
                "monitor": workspace.monitor,
                "windows": workspace.windows,
            })).collect::<Vec<_>>(),
            "error": snapshot.hyprland.error,
        },
        "audio": {
            "available": snapshot.audio.available,
            "outputPercent": snapshot.audio.output_percent,
            "outputMuted": snapshot.audio.output_muted,
            "inputPercent": snapshot.audio.input_percent,
            "inputMuted": snapshot.audio.input_muted,
            "error": snapshot.audio.error,
        },
        "network": {
            "available": snapshot.network.available,
            "device": snapshot.network.device,
            "kind": snapshot.network.kind,
            "connection": snapshot.network.connection,
            "error": snapshot.network.error,
        },
        "bluetooth": {
            "available": snapshot.bluetooth.available,
            "powered": snapshot.bluetooth.powered,
            "connectedDevices": snapshot.bluetooth.connected_devices,
            "error": snapshot.bluetooth.error,
        },
        "battery": {
            "available": snapshot.battery.available,
            "percentage": snapshot.battery.percentage,
            "charging": snapshot.battery.charging,
            "state": snapshot.battery.state,
            "error": snapshot.battery.error,
        },
        "media": {
            "available": snapshot.media.available,
            "player": snapshot.media.player,
            "status": snapshot.media.status,
            "artist": snapshot.media.artist,
            "title": snapshot.media.title,
            "error": snapshot.media.error,
        },
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HyprlandState {
    pub available: bool,
    pub active_workspace: String,
    pub workspaces: Vec<WorkspaceState>,
    pub active_window: String,
    pub active_class: String,
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
    let monitor = monitors
        .as_array()
        .and_then(|items| items.first())
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

    HyprlandState {
        available: monitors.is_array(),
        active_workspace,
        workspaces: parsed_workspaces,
        active_window: active_title,
        active_class,
        monitor: monitor
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        error: None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioState {
    pub available: bool,
    pub output_percent: Option<u8>,
    pub output_muted: bool,
    pub input_percent: Option<u8>,
    pub input_muted: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioEndpoint {
    pub percent: Option<u8>,
    pub muted: bool,
}

impl AudioState {
    fn collect() -> Self {
        let output = command("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).ok();
        let input = command("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"]).ok();
        if output.is_none() && input.is_none() {
            return Self {
                error: Some("wpctl unavailable".to_string()),
                ..Self::default()
            };
        }
        Self {
            available: true,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkState {
    pub available: bool,
    pub device: String,
    pub kind: String,
    pub connection: String,
    pub error: Option<String>,
}

impl NetworkState {
    fn collect() -> Self {
        match command(
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
            Ok(raw) => parse_nmcli_device_status(&raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
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
        };
        if fields[2] == "connected" {
            return candidate;
        }
        fallback = Some(candidate);
    }
    fallback.unwrap_or_default()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothState {
    pub available: bool,
    pub powered: bool,
    pub connected_devices: usize,
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
        let devices = command("bluetoothctl", &["devices", "Connected"])
            .map(|raw| raw.lines().filter(|line| !line.trim().is_empty()).count())
            .unwrap_or_default();
        parse_bluetooth_show(&show, devices)
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
        error: None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatteryState {
    pub available: bool,
    pub percentage: Option<u8>,
    pub charging: bool,
    pub state: String,
    pub error: Option<String>,
}

impl BatteryState {
    fn collect() -> Self {
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
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaState {
    pub available: bool,
    pub player: String,
    pub status: String,
    pub artist: String,
    pub title: String,
    pub error: Option<String>,
}

impl MediaState {
    fn collect() -> Self {
        match command(
            "playerctl",
            &[
                "-a",
                "metadata",
                "--format",
                "{{playerName}}\t{{status}}\t{{artist}}\t{{title}}",
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

pub fn parse_playerctl_metadata(raw: &str) -> MediaState {
    let Some(line) = raw.lines().find(|line| !line.trim().is_empty()) else {
        return MediaState::default();
    };
    let fields = line.split('\t').collect::<Vec<_>>();
    MediaState {
        available: true,
        player: fields.first().copied().unwrap_or_default().to_string(),
        status: fields.get(1).copied().unwrap_or_default().to_string(),
        artist: fields.get(2).copied().unwrap_or_default().to_string(),
        title: fields.get(3).copied().unwrap_or_default().to_string(),
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

fn command_json(program: &str, args: &[&str]) -> Result<Value, String> {
    let raw = command(program, args)?;
    serde_json::from_str(&raw).map_err(|error| format!("{program} returned invalid JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        SystemAction, parse_bluetooth_show, parse_hyprland, parse_nmcli_device_status,
        parse_playerctl_metadata, parse_upower_display, parse_wpctl_volume, run_action,
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
        let active = json!({"title": "Terminal", "class": "foot"});
        let parsed = parse_hyprland(&monitors, Some(&workspaces), Some(&active));
        assert!(parsed.available);
        assert_eq!(parsed.active_workspace, "2");
        assert_eq!(parsed.workspaces[0].id, 1);
        assert_eq!(parsed.active_window, "Terminal");
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
    fn selects_connected_network_device() {
        let parsed = parse_nmcli_device_status(
            "enp1s0:ethernet:disconnected:--\nwlan0:wifi:connected:Home WiFi\n",
        );
        assert_eq!(parsed.device, "wlan0");
        assert_eq!(parsed.connection, "Home WiFi");
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
    fn parses_upower_display_state() {
        let parsed = parse_upower_display("state: charging\npercentage: 87%\n");
        assert!(parsed.available);
        assert!(parsed.charging);
        assert_eq!(parsed.percentage, Some(87));
    }

    #[test]
    fn parses_first_media_player() {
        let parsed = parse_playerctl_metadata("Firefox\tPlaying\tArtist\tTitle\n");
        assert!(parsed.available);
        assert_eq!(parsed.status, "Playing");
        assert_eq!(parsed.title, "Title");
    }

    #[test]
    fn rejects_control_characters_before_shell_action() {
        let result = run_action(&SystemAction::FocusWorkspace(String::from("1\nquit")));
        assert_eq!(result, Err("invalid workspace".to_string()));
    }
}
