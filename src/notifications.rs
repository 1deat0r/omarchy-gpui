//! Freedesktop notification service and Omarchy-compatible persistence.
//!
//! Quickshell owns `org.freedesktop.Notifications` in the reference shell.
//! The GPUI replacement claims the same name when it is available, writes the
//! same state files, and forwards accepted notifications into the GPUI event
//! stream. If another notification daemon is still running, this adapter
//! fails closed and leaves that daemon untouched.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use zbus::{blocking::connection::Builder, object_server::SignalEmitter, zvariant::OwnedValue};

use crate::ipc::IpcEvent;

const NOTIFICATION_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFICATION_NAME: &str = "org.freedesktop.Notifications";
const NORMAL_URGENCY: u8 = 1;
const CRITICAL_URGENCY: u8 = 2;

/// Owns the optional session-bus notification server for the lifetime of the
/// shell. The service is intentionally optional so a running Quickshell or
/// another notification daemon is never replaced behind the user's back.
pub struct NotificationServerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub fn start(user_config_path: &Path, events: Sender<IpcEvent>) -> NotificationServerHandle {
    let home = home_from_config_path(user_config_path);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("omarchy-gpui-notifications".to_string())
        .spawn(move || run_server(home, events, thread_stop))
        .ok();
    NotificationServerHandle { stop, thread }
}

fn run_server(home: PathBuf, events: Sender<IpcEvent>, stop: Arc<AtomicBool>) {
    let service = NotificationService::new(home, events);
    let connection = match Builder::session()
        .and_then(|builder| builder.serve_at(NOTIFICATION_PATH, service))
        .and_then(|builder| builder.name(NOTIFICATION_NAME))
        .and_then(|builder| builder.build())
    {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("omarchy-gpui-shell: notification service unavailable: {error}");
            return;
        }
    };

    while !stop.load(Ordering::Acquire) {
        thread::park_timeout(Duration::from_millis(250));
    }
    drop(connection);
}

impl Drop for NotificationServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

struct NotificationService {
    state: Arc<Mutex<NotificationState>>,
}

impl NotificationService {
    fn new(home: PathBuf, events: Sender<IpcEvent>) -> Self {
        Self {
            state: Arc::new(Mutex::new(NotificationState::new(home, events))),
        }
    }
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationService {
    fn get_capabilities(&self) -> Vec<String> {
        [
            "body",
            "body-markup",
            "body-hyperlinks",
            "icon-static",
            "actions",
            "persistence",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "Omarchy GPUI".to_string(),
            "omarchy-gpui-shell".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            "1.2".to_string(),
        )
    }

    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: std::collections::HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failed("notification state lock poisoned"))?;
        let result = state
            .accept(
                app_name,
                replaces_id,
                app_icon,
                summary,
                body,
                actions,
                &hints,
                expire_timeout,
            )
            .map_err(failed)?;
        if let Some(entry) = result.event {
            let payload = serde_json::to_string(&entry)
                .map_err(|error| failed(format!("serialize notification: {error}")))?;
            let _ = state.events.send(IpcEvent::Notification { entry: payload });
        }
        if let Some((id, path, lifetime)) = result.expiry {
            let state = Arc::clone(&self.state);
            thread::spawn(move || {
                thread::sleep(lifetime);
                if let Ok(mut state) = state.lock() {
                    let _ = state.expire(id, &path);
                }
            });
        }
        Ok(result.id)
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let closed = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failed("notification state lock poisoned"))?;
            state.close(id).map_err(failed)?
        };
        if closed {
            Self::notification_closed(&emitter, id, 3)
                .await
                .map_err(|error| failed(format!("emit NotificationClosed: {error}")))?;
        }
        Ok(())
    }

    async fn invoke_action(
        &self,
        id: u32,
        action_key: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let valid = self
            .state
            .lock()
            .map_err(|_| failed("notification state lock poisoned"))?
            .actions
            .get(&id)
            .is_some_and(|actions| actions.iter().any(|key| key == &action_key));
        if !valid {
            return Err(failed("notification action is unavailable"));
        }
        Self::action_invoked(&emitter, id, action_key)
            .await
            .map_err(|error| failed(format!("emit ActionInvoked: {error}")))
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: String,
    ) -> zbus::Result<()>;
}

struct NotificationState {
    home: PathBuf,
    next_id: u32,
    live: BTreeMap<u32, PathBuf>,
    actions: BTreeMap<u32, Vec<String>>,
    events: Sender<IpcEvent>,
}

struct AcceptedNotification {
    id: u32,
    event: Option<NotificationEntry>,
    expiry: Option<(u32, PathBuf, Duration)>,
}

impl NotificationState {
    fn new(home: PathBuf, events: Sender<IpcEvent>) -> Self {
        let (live, highest_id) = scan_live_notifications(&home);
        Self {
            home,
            next_id: highest_id.saturating_add(1).max(1),
            live,
            actions: BTreeMap::new(),
            events,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn accept(
        &mut self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: &std::collections::HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> Result<AcceptedNotification, String> {
        let replacing_path = (replaces_id != 0)
            .then(|| self.live.get(&replaces_id).cloned())
            .flatten();
        let id = if replacing_path.is_some() {
            replaces_id
        } else {
            self.allocate_id()
        };
        let replacement_timestamp = replacing_path
            .as_deref()
            .and_then(read_notification_timestamp);
        if let Some(old_path) = self.live.remove(&id)
            && replacing_path.as_deref() != Some(old_path.as_path())
        {
            remove_file_if_present(&old_path)?;
        }

        let entry = NotificationEntry {
            id,
            original_id: id,
            app: app_name.clone(),
            app_icon,
            summary,
            body,
            image: string_hint(hints, &["image-path", "image_path"]),
            glyph: string_hint(hints, &["omarchy-glyph"]),
            exec_argv: string_hint(hints, &["omarchy-exec-argv"]),
            urgency: hint_u8(hints, "urgency").unwrap_or(NORMAL_URGENCY),
            expire_timeout: normalize_expire_timeout(expire_timeout),
            timestamp: replacement_timestamp.unwrap_or_else(unix_millis),
            actions: parse_actions(&actions),
        };
        let path = replacing_path.unwrap_or_else(|| live_path(&self.home, &entry));
        write_entry(&path, &entry)?;

        let dnd = load_dnd(&self.home);
        let bypass = should_bypass_dnd(&app_name, entry.urgency);
        let transient = hint_bool(hints, "transient").unwrap_or(false);
        let event = if dnd && !bypass {
            if is_ephemeral(&app_name, transient) {
                remove_file_if_present(&path)?;
                None
            } else {
                move_to_history(&self.home, &path)?;
                None
            }
        } else {
            self.live.insert(id, path);
            self.actions.insert(
                id,
                entry
                    .actions
                    .iter()
                    .map(|action| action.identifier.clone())
                    .collect(),
            );
            Some(entry)
        };

        let expiry = event
            .as_ref()
            .and_then(notification_lifetime)
            .map(|lifetime| {
                let path = self.live.get(&id).cloned().unwrap_or_default();
                (id, path, lifetime)
            });
        Ok(AcceptedNotification { id, event, expiry })
    }

    fn close(&mut self, id: u32) -> Result<bool, String> {
        let Some(path) = self.live.remove(&id) else {
            return Ok(false);
        };
        self.actions.remove(&id);
        move_to_history(&self.home, &path)?;
        Ok(true)
    }

    fn expire(&mut self, id: u32, expected_path: &Path) -> Result<(), String> {
        if self.live.get(&id).is_some_and(|path| path == expected_path)
            && let Some(path) = self.live.remove(&id)
        {
            self.actions.remove(&id);
            move_to_history(&self.home, &path)?;
        }
        Ok(())
    }

    fn allocate_id(&mut self) -> u32 {
        let id = self.next_id.max(1);
        self.next_id = if id == u32::MAX { 1 } else { id + 1 };
        id
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct NotificationEntry {
    id: u32,
    #[serde(rename = "originalId")]
    original_id: u32,
    app: String,
    #[serde(rename = "appIcon")]
    app_icon: String,
    summary: String,
    body: String,
    image: String,
    glyph: String,
    #[serde(rename = "execArgv")]
    exec_argv: String,
    urgency: u8,
    #[serde(rename = "expireTimeout")]
    expire_timeout: u32,
    timestamp: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    actions: Vec<NotificationAction>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct NotificationAction {
    identifier: String,
    label: String,
}

fn parse_actions(values: &[String]) -> Vec<NotificationAction> {
    values
        .chunks_exact(2)
        .filter_map(|pair| {
            let identifier = pair[0].clone();
            let label = pair[1].clone();
            (!identifier.is_empty() && !label.is_empty())
                .then_some(NotificationAction { identifier, label })
        })
        .collect()
}

fn read_notification_timestamp(path: &Path) -> Option<u64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("timestamp").and_then(serde_json::Value::as_u64))
}

fn scan_live_notifications(home: &Path) -> (BTreeMap<u32, PathBuf>, u32) {
    let directory = notifications_dir(home);
    let Ok(entries) = fs::read_dir(directory) else {
        return (BTreeMap::new(), 0);
    };
    let mut live = BTreeMap::new();
    let mut highest_id = 0;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(value) = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        else {
            continue;
        };
        let id = value
            .get("originalId")
            .or_else(|| value.get("id"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id != 0);
        let Some(id) = id else { continue };
        highest_id = highest_id.max(id);
        live.insert(id, path);
    }
    (live, highest_id)
}

fn write_entry(path: &Path, entry: &NotificationEntry) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("notification path has no parent".to_string());
    };
    fs::create_dir_all(parent)
        .map_err(|error| format!("create notification directory: {error}"))?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("notification"),
        std::process::id()
    ));
    let contents = serde_json::to_vec(entry)
        .map_err(|error| format!("serialize notification entry: {error}"))?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("create notification temporary: {error}"))?;
        file.write_all(&contents)
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|error| format!("write notification entry: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync notification entry: {error}"))?;
        fs::rename(&temporary, path).map_err(|error| format!("replace notification entry: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn move_to_history(home: &Path, path: &Path) -> Result<(), String> {
    let history = notifications_dir(home).join("history");
    fs::create_dir_all(&history)
        .map_err(|error| format!("create notification history: {error}"))?;
    let name = path
        .file_name()
        .ok_or_else(|| "notification path has no file name".to_string())?;
    let destination = history.join(name);
    fs::rename(path, destination).map_err(|error| format!("archive notification: {error}"))
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove notification: {error}")),
    }
}

fn notifications_dir(home: &Path) -> PathBuf {
    home.join(".local/state/omarchy/notifications")
}

fn live_path(home: &Path, entry: &NotificationEntry) -> PathBuf {
    notifications_dir(home).join(notification_file_name(entry))
}

fn notification_file_name(entry: &NotificationEntry) -> String {
    format!("{}-{}.json", entry.timestamp, entry.original_id)
}

fn notification_lifetime(entry: &NotificationEntry) -> Option<Duration> {
    if entry.urgency >= CRITICAL_URGENCY && entry.expire_timeout == 0 {
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

fn home_from_config_path(path: &Path) -> PathBuf {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn normalize_expire_timeout(timeout: i32) -> u32 {
    u32::try_from(timeout).unwrap_or_default()
}

fn load_dnd(home: &Path) -> bool {
    fs::read_to_string(home.join(".local/state/omarchy/notifications.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("dnd").cloned())
        .map(|value| match value {
            serde_json::Value::Bool(enabled) => enabled,
            serde_json::Value::String(value) => {
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            }
            _ => false,
        })
        .unwrap_or(false)
}

fn should_bypass_dnd(app_name: &str, urgency: u8) -> bool {
    app_name == "omarchy-action" || (app_name == "notify-send" && urgency == CRITICAL_URGENCY)
}

fn is_ephemeral(app_name: &str, transient: bool) -> bool {
    transient || matches!(app_name, "notify-send" | "omarchy-action")
}

fn string_hint(hints: &std::collections::HashMap<String, OwnedValue>, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| {
            hints.get(*name).and_then(|value| {
                value
                    .try_clone()
                    .ok()
                    .and_then(|value| String::try_from(value).ok())
            })
        })
        .unwrap_or_default()
}

fn hint_u8(hints: &std::collections::HashMap<String, OwnedValue>, name: &str) -> Option<u8> {
    hints
        .get(name)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| u8::try_from(value).ok())
}

fn hint_bool(hints: &std::collections::HashMap<String, OwnedValue>, name: &str) -> Option<bool> {
    hints
        .get(name)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| bool::try_from(value).ok())
}

fn failed(message: impl Into<String>) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(message.into())
}

#[cfg(test)]
mod tests {
    use super::{
        CRITICAL_URGENCY, NORMAL_URGENCY, NotificationEntry, is_ephemeral,
        normalize_expire_timeout, notification_file_name, parse_actions, should_bypass_dnd,
    };

    #[test]
    fn dnd_bypass_matches_reference_sender_rules() {
        assert!(should_bypass_dnd("omarchy-action", NORMAL_URGENCY));
        assert!(should_bypass_dnd("notify-send", CRITICAL_URGENCY));
        assert!(!should_bypass_dnd("chat-app", CRITICAL_URGENCY));
        assert!(!should_bypass_dnd("notify-send", NORMAL_URGENCY));
    }

    #[test]
    fn ephemeral_notifications_are_not_recorded_when_silenced() {
        assert!(is_ephemeral("notify-send", false));
        assert!(is_ephemeral("omarchy-action", false));
        assert!(is_ephemeral("mail", true));
        assert!(!is_ephemeral("mail", false));
    }

    #[test]
    fn expiration_normalization_is_fail_closed() {
        assert_eq!(normalize_expire_timeout(-1), 0);
        assert_eq!(normalize_expire_timeout(5000), 5000);
    }

    #[test]
    fn persisted_file_name_uses_timestamp_and_original_id() {
        let entry = NotificationEntry {
            id: 7,
            original_id: 7,
            app: String::new(),
            app_icon: String::new(),
            summary: String::new(),
            body: String::new(),
            image: String::new(),
            glyph: String::new(),
            exec_argv: String::new(),
            urgency: NORMAL_URGENCY,
            expire_timeout: 0,
            timestamp: 123,
            actions: Vec::new(),
        };
        assert_eq!(notification_file_name(&entry), "123-7.json");
    }

    #[test]
    fn notification_actions_preserve_identifier_label_pairs() {
        let actions = parse_actions(&[
            "default".to_string(),
            "Open".to_string(),
            "snooze".to_string(),
            "Snooze".to_string(),
            "dangling".to_string(),
        ]);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].identifier, "default");
        assert_eq!(actions[1].label, "Snooze");
    }
}
