//! Typed adapters for Omarchy plugins that are not compositor primitives.
//!
//! Quickshell exposes these values as live service objects.  GPUI owns the
//! equivalent boundary here: each adapter runs the reference command (or reads
//! the same state file), parses its output, and keeps an unavailable service
//! unavailable instead of inventing a value for the bar.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

use crate::dbus::{self, TraySnapshot};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginSnapshot {
    pub agents: AgentState,
    pub update: UpdateState,
    pub keyboard: KeyboardLayoutState,
    pub weather: WeatherState,
    pub idle: IdleState,
    pub indicators: IndicatorState,
    pub dropbox: DropboxState,
    pub tailscale: TailscaleState,
    pub tray: TraySnapshot,
}

impl PluginSnapshot {
    pub fn collect(omarchy_path: &Path) -> Self {
        Self {
            agents: AgentState::collect(omarchy_path),
            update: UpdateState::collect(omarchy_path),
            keyboard: KeyboardLayoutState::collect(),
            weather: WeatherState::collect(omarchy_path),
            idle: IdleState::collect(),
            indicators: IndicatorState::collect(),
            dropbox: DropboxState::collect_dropbox(omarchy_path),
            tailscale: TailscaleState::collect(),
            tray: match dbus::tray_snapshot() {
                Ok(tray) => tray,
                Err(error) => TraySnapshot {
                    error: Some(error),
                    ..TraySnapshot::default()
                },
            },
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentState {
    pub default_agent: String,
    pub available: bool,
    pub error: Option<String>,
}

impl AgentState {
    fn collect(omarchy_path: &Path) -> Self {
        match omarchy_command(omarchy_path, "omarchy-default-agent", &[]) {
            Ok(raw) => parse_default_agent(&raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }
}

pub fn parse_default_agent(raw: &str) -> AgentState {
    let default_agent = raw.trim().to_string();
    AgentState {
        available: !default_agent.is_empty(),
        default_agent,
        error: None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateState {
    pub available: bool,
    pub detail: String,
    pub error: Option<String>,
}

impl UpdateState {
    fn collect(omarchy_path: &Path) -> Self {
        match omarchy_command_with_status(omarchy_path, "omarchy-update-available", &[]) {
            Ok((success, raw)) => parse_update_status(success, &raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }
}

pub fn parse_update_status(success: bool, raw: &str) -> UpdateState {
    let detail = raw.trim().to_string();
    UpdateState {
        available: success,
        detail,
        error: None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyboardLayoutState {
    pub available: bool,
    pub multiple_layouts: bool,
    pub keyboard_name: String,
    pub layout_full: String,
    pub layout_label: String,
    pub error: Option<String>,
}

impl KeyboardLayoutState {
    fn collect() -> Self {
        match command_output("hyprctl", &["-j", "devices"]) {
            Ok(raw) => parse_keyboard_devices(&raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }
}

pub fn parse_keyboard_devices(raw: &str) -> KeyboardLayoutState {
    let parsed = match serde_json::from_str::<Value>(raw) {
        Ok(value) => value,
        Err(error) => {
            return KeyboardLayoutState {
                error: Some(format!("hyprctl devices returned invalid JSON: {error}")),
                ..KeyboardLayoutState::default()
            };
        }
    };
    let mut keyboards = parsed
        .get("keyboards")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|keyboard| {
            let name = keyboard
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            !is_untyped_keyboard(name)
        })
        .filter_map(|keyboard| {
            let name = keyboard.get("name")?.as_str()?.to_string();
            let layout = keyboard
                .get("active_keymap")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let index = keyboard
                .get("active_layout_index")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let layouts = keyboard
                .get("layout")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((name, layout, index, layouts.to_string()))
        })
        .collect::<Vec<_>>();

    keyboards.sort_by_key(|keyboard| keyboard.2);
    let Some((keyboard_name, layout_full, _, layouts)) = keyboards.pop() else {
        return KeyboardLayoutState::default();
    };
    let multiple_layouts =
        layouts.contains(',') || keyboards.iter().any(|keyboard| keyboard.3.contains(','));
    let layout_label = layout_full
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .chars()
        .take(3)
        .collect::<String>()
        .to_uppercase();
    KeyboardLayoutState {
        available: !layout_full.is_empty(),
        multiple_layouts,
        keyboard_name,
        layout_full,
        layout_label,
        error: None,
    }
}

fn is_untyped_keyboard(name: &str) -> bool {
    [
        "hl-virtual-keyboard",
        "power-button",
        "sleep-button",
        "lid-switch",
        "video-bus",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WeatherState {
    pub available: bool,
    pub status: String,
    pub location: String,
    pub error: Option<String>,
}

impl WeatherState {
    fn collect(omarchy_path: &Path) -> Self {
        let status = match omarchy_command(omarchy_path, "omarchy-weather-status", &[]) {
            Ok(raw) => raw.trim().to_string(),
            Err(error) => {
                return Self {
                    error: Some(error),
                    ..Self::default()
                };
            }
        };
        parse_weather_status(&status)
    }
}

pub fn parse_weather_status(raw: &str) -> WeatherState {
    let status = raw.trim().to_string();
    let location = status
        .split("·")
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    WeatherState {
        available: !status.is_empty() && !status.eq_ignore_ascii_case("weather unavailable"),
        status,
        location,
        error: None,
    }
}

/// Parse the metadata header and compact 0/1 matrix emitted by
/// `omarchy-network-qr --meta`.
///
/// The QR command deliberately keeps the password inside the encoded matrix;
/// only the interface, security mode, and SSID are exposed as display metadata.
pub fn parse_network_qr(raw: &str) -> (String, Vec<String>) {
    let mut meta = String::new();
    let mut rows = Vec::new();
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(rest) = line.strip_prefix("meta\t") {
            let mut fields = rest.splitn(3, '\t');
            let interface = fields.next().unwrap_or_default();
            let security = fields.next().unwrap_or_default();
            let ssid = fields.next().unwrap_or_default();
            meta = match (interface.is_empty(), security.is_empty(), ssid.is_empty()) {
                (false, false, false) => format!("Wi-Fi: {ssid} · {security} · {interface}"),
                (false, false, true) => format!("Wi-Fi · {security} · {interface}"),
                (false, true, false) => format!("Wi-Fi: {ssid} · {interface}"),
                _ => "Wi-Fi connection".to_string(),
            };
        } else if line.chars().all(|character| matches!(character, '0' | '1')) {
            rows.push(line.to_string());
        }
    }
    (meta, rows)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdleState {
    pub enabled: bool,
    pub stay_awake: bool,
    pub state_loaded: bool,
    pub state_path: PathBuf,
    pub error: Option<String>,
}

impl IdleState {
    fn collect() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let state_path = home.join(".local/state/omarchy/indicators/stay-awake");
        let stay_awake = match fs::metadata(&state_path) {
            Ok(metadata) => metadata.is_file(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Self {
                    state_path,
                    error: Some(format!("read idle state: {error}")),
                    ..Self::default()
                };
            }
        };
        Self {
            enabled: !stay_awake,
            stay_awake,
            state_loaded: true,
            state_path,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndicatorState {
    pub dictation: String,
    pub recording: bool,
    pub reminder_count: u64,
    pub reminder_tooltip: String,
    pub dnd: bool,
    pub stay_awake: bool,
    pub error: Option<String>,
}

impl IndicatorState {
    fn collect() -> Self {
        let dictation = command_output("omarchy-voxtype-status", &[])
            .ok()
            .and_then(|raw| parse_dictation_state(&raw))
            .unwrap_or_default();
        let recording = Command::new("pgrep")
            .args(["--quiet", "-f", "^gpu-screen-recorder"])
            .status()
            .is_ok_and(|status| status.success());
        let (reminder_count, reminder_tooltip) =
            command_output("omarchy-reminder", &["show", "--json"])
                .ok()
                .map(|raw| parse_reminder_indicator(&raw))
                .unwrap_or_default();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let dnd = fs::read_to_string(home.join(".local/state/omarchy/notifications.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| value.get("dnd").and_then(Value::as_bool))
            .unwrap_or(false);
        let stay_awake = fs::metadata(home.join(".local/state/omarchy/indicators/stay-awake"))
            .is_ok_and(|metadata| metadata.is_file());
        Self {
            dictation,
            recording,
            reminder_count,
            reminder_tooltip,
            dnd,
            stay_awake,
            error: None,
        }
    }
}

pub fn parse_dictation_state(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    Some(
        value
            .get("alt")
            .or_else(|| value.get("class"))
            .and_then(Value::as_str)
            .unwrap_or("idle")
            .to_string(),
    )
}

pub fn parse_reminder_indicator(raw: &str) -> (u64, String) {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return (0, String::new());
    };
    (
        value
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        value
            .get("tooltip")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DropboxState {
    pub installed: bool,
    pub running: bool,
    pub authenticated: bool,
    pub status_text: String,
    pub account_path: String,
    pub plan: String,
    pub used_bytes: u64,
    pub quota_bytes: u64,
    pub usage_percent: f64,
    pub quota_known: bool,
    pub error: Option<String>,
}

impl DropboxState {
    fn collect_dropbox(omarchy_path: &Path) -> Self {
        let helper = omarchy_path.join("shell/plugins/panels/dropbox/status.py");
        if !helper.is_file() {
            return Self {
                status_text: "Unavailable".to_string(),
                ..Self::default()
            };
        }
        match Command::new("python3").arg(&helper).arg("25").output() {
            Ok(output) if output.status.success() => {
                parse_dropbox_status(&String::from_utf8_lossy(&output.stdout))
            }
            Ok(output) => {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
                Self {
                    status_text: "Unavailable".to_string(),
                    error: Some(if detail.is_empty() {
                        format!("{} exited unsuccessfully", helper.display())
                    } else {
                        detail
                    }),
                    ..Self::default()
                }
            }
            Err(error) => Self {
                status_text: "Unavailable".to_string(),
                error: Some(format!("{}: {error}", helper.display())),
                ..Self::default()
            },
        }
    }
}

pub fn parse_dropbox_status(raw: &str) -> DropboxState {
    let Ok(value) = serde_json::from_str::<Value>(raw.trim()) else {
        return DropboxState {
            status_text: "Unavailable".to_string(),
            error: Some("Failed to parse Dropbox status".to_string()),
            ..DropboxState::default()
        };
    };
    let Some(object) = value.as_object() else {
        return DropboxState {
            status_text: "Unavailable".to_string(),
            error: Some("Dropbox status was not an object".to_string()),
            ..DropboxState::default()
        };
    };
    let installed = object
        .get("installed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status_text = object
        .get("statusText")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(if installed {
            "Stopped"
        } else {
            "Not installed"
        })
        .to_string();
    DropboxState {
        installed,
        running: object
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        authenticated: object
            .get("authenticated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status_text,
        account_path: object
            .get("accountPath")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        plan: object
            .get("plan")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        used_bytes: object
            .get("usedBytes")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        quota_bytes: object
            .get("quotaBytes")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        usage_percent: object
            .get("usagePercent")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        quota_known: object
            .get("quotaKnown")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        error: None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TailscaleState {
    pub installed: bool,
    pub running: bool,
    pub needs_login: bool,
    pub backend_state: String,
    pub status: String,
    pub self_name: String,
    pub peers: usize,
    pub error: Option<String>,
}

impl TailscaleState {
    fn collect() -> Self {
        match command_output("tailscale", &["status", "--json"]) {
            Ok(raw) => parse_tailscale_status(&raw),
            Err(error) if error.contains("No such file") || error.contains("not found") => Self {
                status: "Not installed".to_string(),
                ..Self::default()
            },
            Err(error) => Self {
                installed: true,
                error: Some(error),
                ..Self::default()
            },
        }
    }
}

pub fn parse_tailscale_status(raw: &str) -> TailscaleState {
    let parsed = match serde_json::from_str::<Value>(raw) {
        Ok(value) => value,
        Err(error) => {
            return TailscaleState {
                installed: true,
                error: Some(format!("tailscale status returned invalid JSON: {error}")),
                ..TailscaleState::default()
            };
        }
    };
    let backend_state = parsed
        .get("BackendState")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let self_name = parsed
        .get("Self")
        .and_then(|value| value.get("HostName"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let peers = parsed
        .get("Peer")
        .and_then(Value::as_object)
        .map_or(0, |peer_map| peer_map.len());
    let needs_login = !parsed
        .get("AuthURL")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .is_empty()
        && backend_state.eq_ignore_ascii_case("NeedsLogin");
    let running = backend_state.eq_ignore_ascii_case("Running");
    let status = if needs_login {
        "Needs login"
    } else if running {
        "Connected"
    } else if backend_state.eq_ignore_ascii_case("Stopped") {
        "Disconnected"
    } else {
        backend_state.as_str()
    }
    .to_string();
    TailscaleState {
        installed: true,
        running,
        needs_login,
        backend_state,
        status,
        self_name,
        peers,
        error: None,
    }
}

fn omarchy_command(path: &Path, name: &str, args: &[&str]) -> Result<String, String> {
    let (success, output) = omarchy_command_with_status(path, name, args)?;
    if success {
        Ok(output)
    } else {
        Err(format!("{name} exited unsuccessfully: {}", output.trim()))
    }
}

fn omarchy_command_with_status(
    path: &Path,
    name: &str,
    args: &[&str],
) -> Result<(bool, String), String> {
    let bundled = path.join("bin").join(name);
    let program = if bundled.is_file() {
        bundled
    } else {
        PathBuf::from(name)
    };
    let output = Command::new(&program)
        .args(args)
        .output()
        .map_err(|error| format!("{}: {error}", program.display()))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    Ok((output.status.success(), text))
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

#[cfg(test)]
mod tests {
    use super::{
        parse_default_agent, parse_dictation_state, parse_dropbox_status, parse_keyboard_devices,
        parse_network_qr, parse_reminder_indicator, parse_tailscale_status, parse_update_status,
        parse_weather_status,
    };

    #[test]
    fn parses_plugin_status_boundaries() {
        assert_eq!(parse_default_agent("pi\n").default_agent, "pi");
        let update = parse_update_status(true, "omarchy 1 new commit\n");
        assert!(update.available);
        assert_eq!(update.detail, "omarchy 1 new commit");
        let weather = parse_weather_status("Auckland  ·  Temp 12°C  ·  Wind ←4km/h");
        assert!(weather.available);
        assert_eq!(weather.location, "Auckland");
    }

    #[test]
    fn keyboard_parser_ignores_non_typing_devices_and_selects_furthest_layout() {
        let state = parse_keyboard_devices(
            r#"{"keyboards":[{"name":"power-button","active_keymap":"us","active_layout_index":9,"layout":"us"},{"name":"at-translated-set-2-keyboard","active_keymap":"English (US)","active_layout_index":1,"layout":"us,gb"}]}"#,
        );
        assert_eq!(state.keyboard_name, "at-translated-set-2-keyboard");
        assert_eq!(state.layout_label, "ENG");
        assert!(state.multiple_layouts);
    }

    #[test]
    fn tailscale_parser_reports_backend_and_peer_count() {
        let state = parse_tailscale_status(
            r#"{"BackendState":"Running","Self":{"HostName":"laptop"},"Peer":{"a":{},"b":{}}}"#,
        );
        assert!(state.installed);
        assert!(state.running);
        assert_eq!(state.status, "Connected");
        assert_eq!(state.peers, 2);
    }

    #[test]
    fn network_qr_parser_keeps_metadata_and_matrix_separate() {
        let (meta, rows) = parse_network_qr("meta\twlp6s0\tWPA\tSTARLINK\n0101\n1110\n");
        assert_eq!(meta, "Wi-Fi: STARLINK · WPA · wlp6s0");
        assert_eq!(rows, vec!["0101", "1110"]);
    }

    #[test]
    fn dropbox_parser_preserves_status_and_quota_fields() {
        let state = parse_dropbox_status(
            r#"{"ok":true,"installed":true,"running":true,"authenticated":true,"statusText":"Up to date","accountPath":"/home/me/Dropbox","plan":"basic","usedBytes":1200,"quotaBytes":2000,"usagePercent":60,"quotaKnown":true}"#,
        );
        assert!(state.installed);
        assert!(state.running);
        assert!(state.authenticated);
        assert_eq!(state.status_text, "Up to date");
        assert_eq!(state.used_bytes, 1200);
        assert_eq!(state.quota_bytes, 2000);
        assert!(state.quota_known);
    }

    #[test]
    fn indicator_parsers_preserve_reference_states() {
        assert_eq!(
            parse_dictation_state(r#"{"alt":"transcribing","class":"busy"}"#),
            Some("transcribing".to_string())
        );
        assert_eq!(
            parse_reminder_indicator(r#"{"count":2,"tooltip":"Due soon"}"#),
            (2, "Due soon".to_string())
        );
        assert_eq!(parse_reminder_indicator("bad"), (0, String::new()));
    }
}
