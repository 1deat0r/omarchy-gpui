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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginSnapshot {
    pub agents: AgentState,
    pub update: UpdateState,
    pub keyboard: KeyboardLayoutState,
    pub weather: WeatherState,
    pub idle: IdleState,
    pub dropbox: ServicePresence,
    pub tailscale: TailscaleState,
}

impl PluginSnapshot {
    pub fn collect(omarchy_path: &Path) -> Self {
        Self {
            agents: AgentState::collect(omarchy_path),
            update: UpdateState::collect(omarchy_path),
            keyboard: KeyboardLayoutState::collect(),
            weather: WeatherState::collect(omarchy_path),
            idle: IdleState::collect(),
            dropbox: ServicePresence::collect_dropbox(omarchy_path),
            tailscale: TailscaleState::collect(),
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
pub struct ServicePresence {
    pub installed: bool,
    pub running: bool,
    pub error: Option<String>,
}

impl ServicePresence {
    fn collect_dropbox(omarchy_path: &Path) -> Self {
        if !command_present("dropbox-cli") && !command_present("dropbox") {
            return Self::default();
        }
        match omarchy_command_with_status(omarchy_path, "omarchy-installed-service-dropbox", &[]) {
            Ok((running, _)) => Self {
                installed: true,
                running,
                error: None,
            },
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
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
            Err(error) if error.contains("No such file") || error.contains("not found") => {
                Self::default()
            }
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

fn command_present(program: &str) -> bool {
    Command::new("which")
        .arg(program)
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::{
        parse_default_agent, parse_keyboard_devices, parse_tailscale_status, parse_update_status,
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
}
