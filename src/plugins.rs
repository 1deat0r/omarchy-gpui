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
    time::{SystemTime, UNIX_EPOCH},
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentState {
    pub default_agent: String,
    pub available: bool,
    pub providers: Vec<AgentProviderState>,
    pub error: Option<String>,
}

impl AgentState {
    fn collect(omarchy_path: &Path) -> Self {
        let mut state = match omarchy_command(omarchy_path, "omarchy-default-agent", &[]) {
            Ok(raw) => parse_default_agent(&raw),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        };
        let usage_dir = agent_usage_dir();
        let mut providers = Vec::new();
        match fs::read_dir(&usage_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    let Ok(raw) = fs::read_to_string(&path) else {
                        continue;
                    };
                    if let Ok(provider) = parse_agent_usage_record(&raw) {
                        providers.push(provider);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                state.error = Some(format!("read {}: {error}", usage_dir.display()));
            }
        }
        providers.sort_by(|left, right| left.id.cmp(&right.id));
        if !providers.is_empty() {
            state.available = true;
            state.error = None;
        }
        state.providers = providers;
        state
    }
}

pub fn parse_default_agent(raw: &str) -> AgentState {
    let default_agent = raw.trim().to_string();
    AgentState {
        available: !default_agent.is_empty(),
        default_agent,
        error: None,
        providers: Vec::new(),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentProviderState {
    pub id: String,
    pub name: String,
    pub ready: bool,
    pub status_text: String,
    pub auth_help_text: String,
    pub tier_label: String,
    pub limits: Vec<AgentLimitState>,
    pub recent_days: Vec<AgentUsageDay>,
    pub models: Vec<AgentModelState>,
    pub today_prompts: u64,
    pub today_sessions: u64,
    pub today_tokens: u64,
    pub total_prompts: u64,
    pub total_sessions: u64,
    pub active_days: u64,
    pub balance: Option<AgentBalanceState>,
    pub scope: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentLimitState {
    pub label: String,
    pub title: String,
    pub percent: f64,
    pub resets_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentUsageDay {
    pub date: String,
    pub message_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentModelState {
    pub id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl AgentModelState {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentBalanceState {
    pub remaining: f64,
    pub funded: f64,
    pub spent: f64,
    pub currency: String,
    pub estimated: bool,
}

pub fn parse_agent_usage_record(raw: &str) -> Result<AgentProviderState, String> {
    let value = serde_json::from_str::<Value>(raw)
        .map_err(|error| format!("agent usage record is invalid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "agent usage record is not an object".to_string())?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if id.is_empty() {
        return Err("agent usage record has no provider id".to_string());
    }
    let mut limits = object
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|limit| {
            let percent = limit.get("percent").and_then(Value::as_f64)?;
            Some(AgentLimitState {
                label: value_string(limit.get("label")),
                title: value_string(limit.get("title")),
                percent: percent.clamp(0.0, 1.0),
                resets_at: value_string(limit.get("resetsAt")),
            })
        })
        .collect::<Vec<_>>();
    limits.sort_by(|left, right| {
        right
            .percent
            .partial_cmp(&left.percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let recent_days = object
        .get("recentDays")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|day| AgentUsageDay {
            date: value_string(day.get("date")),
            message_count: value_u64(day.get("messageCount")),
        })
        .filter(|day| !day.date.is_empty())
        .collect::<Vec<_>>();

    let mut models = object
        .get("modelUsage")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(id, model)| AgentModelState {
            id: id.clone(),
            input_tokens: value_u64(model.get("inputTokens")),
            output_tokens: value_u64(model.get("outputTokens")),
            cache_read_tokens: value_u64(model.get("cacheReadInputTokens")),
            cache_creation_tokens: value_u64(model.get("cacheCreationInputTokens")),
        })
        .collect::<Vec<_>>();
    models.sort_by_key(|model| std::cmp::Reverse(model.total_tokens()));
    models.truncate(4);

    let balance = object.get("balance").and_then(|balance| {
        let remaining = balance.get("remaining").and_then(Value::as_f64)?;
        Some(AgentBalanceState {
            remaining: remaining.max(0.0),
            funded: balance
                .get("funded")
                .and_then(Value::as_f64)
                .unwrap_or_default()
                .max(0.0),
            spent: balance
                .get("spent")
                .and_then(Value::as_f64)
                .unwrap_or_default()
                .max(0.0),
            currency: value_string(balance.get("currency")),
            estimated: balance
                .get("estimated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    });

    Ok(AgentProviderState {
        id,
        name: value_string(object.get("name")),
        ready: object
            .get("ready")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status_text: value_string(object.get("usageStatusText")),
        auth_help_text: value_string(object.get("authHelpText")),
        tier_label: value_string(object.get("tierLabel")),
        limits,
        recent_days,
        models,
        today_prompts: value_u64(object.get("todayPrompts")),
        today_sessions: value_u64(object.get("todaySessions")),
        today_tokens: value_u64(object.get("todayTotalTokens")),
        total_prompts: value_u64(object.get("totalPrompts")),
        total_sessions: value_u64(object.get("totalSessions")),
        active_days: value_u64(object.get("activeDays")),
        balance,
        scope: value_string(object.get("scope")),
        error: None,
    })
}

fn agent_usage_dir() -> PathBuf {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/state"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    state_home.join("omarchy/agents/usage")
}

fn value_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.trim().to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn value_u64(value: Option<&Value>) -> u64 {
    value
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_f64().map(|value| value.max(0.0) as u64))
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or_default()
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
    pub country: String,
    pub condition: String,
    pub icon: String,
    pub temp_c: String,
    pub temp_f: String,
    pub feels_c: String,
    pub feels_f: String,
    pub wind_kmph: String,
    pub wind_mph: String,
    pub humidity: String,
    pub forecast: Vec<WeatherDay>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WeatherDay {
    pub date: String,
    pub max_c: String,
    pub min_c: String,
    pub max_f: String,
    pub min_f: String,
    pub icon: String,
}

impl WeatherState {
    fn collect(omarchy_path: &Path) -> Self {
        let fallback =
            match omarchy_command_with_status(omarchy_path, "omarchy-weather-status", &[]) {
                Ok((success, raw)) if success => parse_weather_status(&raw),
                Ok((_, raw)) => WeatherState {
                    error: Some(format!("omarchy-weather-status: {}", raw.trim())),
                    ..WeatherState::default()
                },
                Err(error) => WeatherState {
                    error: Some(error),
                    ..WeatherState::default()
                },
            };

        match fetch_weather_report(omarchy_path).and_then(|raw| parse_weather_report(&raw)) {
            Ok(mut report) => {
                if report.status.is_empty() {
                    report.status = fallback.status;
                }
                report
            }
            Err(error) => WeatherState {
                error: Some(error),
                ..fallback
            },
        }
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
        ..WeatherState::default()
    }
}

pub fn parse_weather_report(raw: &str) -> Result<WeatherState, String> {
    let parsed = serde_json::from_str::<Value>(raw)
        .map_err(|error| format!("weather report returned invalid JSON: {error}"))?;
    let current = parsed
        .get("current_condition")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .ok_or_else(|| "weather report has no current conditions".to_string())?;

    let area = parsed
        .get("nearest_area")
        .and_then(Value::as_array)
        .and_then(|values| values.first());
    let location = nested_value_text(area, &["areaName", "0", "value"]);
    let country = nested_value_text(area, &["country", "0", "value"]);
    let condition = nested_value_text(Some(current), &["weatherDesc", "0", "value"]);
    let temp_c = value_text(current.get("temp_C"));
    let temp_f = value_text(current.get("temp_F"));
    let feels_c = value_text(current.get("FeelsLikeC"));
    let feels_f = value_text(current.get("FeelsLikeF"));
    let wind_kmph = value_text(current.get("windspeedKmph"));
    let wind_mph = value_text(current.get("windspeedMiles"));
    let humidity = value_text(current.get("humidity"));
    let weather_code = value_text(current.get("weatherCode"));
    let icon = weather_icon(&weather_code, false);
    let mut forecast = Vec::new();
    let today = local_date_string();
    if let Some(days) = parsed.get("weather").and_then(Value::as_array) {
        for day in days {
            let date = value_text(day.get("date"));
            if date.is_empty() || (!today.is_empty() && date <= today) {
                continue;
            }
            let hourly = day
                .get("hourly")
                .and_then(Value::as_array)
                .into_iter()
                .flatten();
            let mut best_hour: Option<&Value> = None;
            let mut best_distance = i64::MAX;
            for hour in hourly {
                let time = value_text(hour.get("time"));
                let numeric = time.parse::<i64>().unwrap_or_default();
                let distance = (numeric - 1200).abs();
                if distance < best_distance {
                    best_distance = distance;
                    best_hour = Some(hour);
                }
            }
            let day_code = best_hour
                .and_then(|hour| hour.get("weatherCode"))
                .map(|value| value_text(Some(value)))
                .unwrap_or_default();
            forecast.push(WeatherDay {
                date,
                max_c: value_text(day.get("maxtempC")),
                min_c: value_text(day.get("mintempC")),
                max_f: value_text(day.get("maxtempF")),
                min_f: value_text(day.get("mintempF")),
                icon: weather_icon(&day_code, false),
            });
            if forecast.len() == 3 {
                break;
            }
        }
    }

    let location = if location.is_empty() {
        "Unknown location".to_string()
    } else {
        location
    };
    let status = if !temp_c.is_empty() || !wind_kmph.is_empty() {
        format!(
            "{location}  ·  Temp {}°C  ·  Wind {} km/h",
            temp_c, wind_kmph
        )
    } else {
        location.clone()
    };

    Ok(WeatherState {
        available: true,
        status,
        location,
        country,
        condition,
        icon,
        temp_c,
        temp_f,
        feels_c,
        feels_f,
        wind_kmph,
        wind_mph,
        humidity,
        forecast,
        error: None,
    })
}

fn fetch_weather_report(omarchy_path: &Path) -> Result<String, String> {
    let query = weather_location_query(omarchy_path);
    let url = format!("https://wttr.in/{query}?format=j1");
    let output = Command::new("curl")
        .args(["-fsS", "--max-time", "10", &url])
        .output()
        .map_err(|error| format!("curl weather report: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("curl weather report exited with {}", output.status)
        } else {
            format!("curl weather report: {detail}")
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn weather_location_query(omarchy_path: &Path) -> String {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let path = home.join(".local/state/omarchy/settings/weather.json");
    let Ok(raw) = fs::read_to_string(path) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return String::new();
    };
    let latitude = value.get("latitude").and_then(Value::as_f64);
    let longitude = value.get("longitude").and_then(Value::as_f64);
    if let (Some(latitude), Some(longitude)) = (latitude, longitude) {
        return format!("{latitude},{longitude}");
    }
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        let _ = omarchy_path;
        String::new()
    } else {
        percent_encode(name)
    }
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn nested_value_text(value: Option<&Value>, path: &[&str]) -> String {
    let mut current = value;
    for segment in path {
        current = current.and_then(|value| {
            value.get(*segment).or_else(|| {
                segment
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| value.get(index))
            })
        });
    }
    value_text(current)
}

fn weather_icon(code: &str, night: bool) -> String {
    match code.parse::<u16>().unwrap_or_default() {
        113 => {
            if night {
                ""
            } else {
                ""
            }
        }
        116 => {
            if night {
                ""
            } else {
                ""
            }
        }
        119 | 122 => "",
        143 | 248 | 260 => "",
        176 | 263 | 353 => {
            if night {
                ""
            } else {
                ""
            }
        }
        179 | 227 | 230 | 323 | 326 | 368 => {
            if night {
                ""
            } else {
                ""
            }
        }
        182 | 185 | 281 | 284 | 311 | 314 | 317 | 320 | 350 | 362 | 365 | 374 | 377 => "",
        200 | 386 | 389 | 392 | 395 => "",
        266 | 293 | 296 | 299 | 302 | 305 | 308 | 356 | 359 => "",
        329 | 332 | 335 | 338 | 371 => "",
        _ => "",
    }
    .to_string()
}

fn local_date_string() -> String {
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
            return format!(
                "{:04}-{:02}-{:02}",
                local.tm_year + 1900,
                local.tm_mon + 1,
                local.tm_mday
            );
        }
    }
    String::new()
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
        parse_agent_usage_record, parse_default_agent, parse_dictation_state, parse_dropbox_status,
        parse_keyboard_devices, parse_network_qr, parse_reminder_indicator, parse_tailscale_status,
        parse_update_status, parse_weather_report, parse_weather_status,
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
    fn parses_agent_usage_limits_history_and_models() {
        let provider = parse_agent_usage_record(
            r#"{
                "id":"codex",
                "name":"Codex",
                "ready":true,
                "usageStatusText":"Ready",
                "tierLabel":"Pro",
                "limits":[{"label":"Weekly","percent":0.42,"resetsAt":"2099-01-01T00:00:00Z"}],
                "recentDays":[{"date":"2098-12-31","messageCount":12}],
                "modelUsage":{"gpt-5":{"inputTokens":100,"outputTokens":50,"cacheReadInputTokens":25}},
                "todayPrompts":4,
                "todaySessions":2,
                "todayTotalTokens":175,
                "totalPrompts":10,
                "totalSessions":3,
                "activeDays":2
            }"#,
        )
        .expect("agent usage fixture should parse");
        assert_eq!(provider.id, "codex");
        assert_eq!(provider.limits[0].title, "");
        assert_eq!(provider.limits[0].percent, 0.42);
        assert_eq!(provider.recent_days[0].message_count, 12);
        assert_eq!(provider.models[0].total_tokens(), 175);
        assert_eq!(provider.today_tokens, 175);
    }

    #[test]
    fn parses_weather_report_current_conditions_and_future_forecast() {
        let weather = parse_weather_report(
            r#"{
                "nearest_area":[{"areaName":[{"value":"Auckland"}],"country":[{"value":"New Zealand"}]}],
                "current_condition":[{"temp_C":"15","temp_F":"59","FeelsLikeC":"14","FeelsLikeF":"57","windspeedKmph":"12","windspeedMiles":"7","humidity":"70","weatherCode":"116","weatherDesc":[{"value":"Partly cloudy"}]}],
                "weather":[
                    {"date":"2099-01-01","maxtempC":"20","mintempC":"12","maxtempF":"68","mintempF":"54","hourly":[{"time":"1200","weatherCode":"113"}]},
                    {"date":"2099-01-02","maxtempC":"21","mintempC":"13","maxtempF":"70","mintempF":"55","hourly":[{"time":"1200","weatherCode":"266"}]}
                ]
            }"#,
        )
        .expect("weather fixture should parse");
        assert_eq!(weather.location, "Auckland");
        assert_eq!(weather.country, "New Zealand");
        assert_eq!(weather.temp_c, "15");
        assert_eq!(weather.condition, "Partly cloudy");
        assert_eq!(weather.icon, "");
        assert_eq!(weather.forecast.len(), 2);
        assert_eq!(weather.forecast[0].date, "2099-01-01");
        assert_eq!(weather.forecast[1].icon, "");
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
