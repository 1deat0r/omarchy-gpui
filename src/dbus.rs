//! Small blocking D-Bus adapters used by the shell services.
//!
//! Omarchy's reference media service is backed by Quickshell's MPRIS model,
//! not by a mandatory `playerctl` executable.  This module keeps the GPUI
//! boundary equivalent: discover session-bus players, read their standard
//! MPRIS properties, and invoke only the methods exposed by each player.

use std::{collections::HashMap, sync::OnceLock};

use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedObjectPath, OwnedValue},
};

const DBUS_DESTINATION: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const MPRIS_PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const STATUS_NOTIFIER_WATCHER_DESTINATION: &str = "org.kde.StatusNotifierWatcher";
const STATUS_NOTIFIER_WATCHER_PATH: &str = "/StatusNotifierWatcher";
const STATUS_NOTIFIER_WATCHER_INTERFACE: &str = "org.kde.StatusNotifierWatcher";
const STATUS_NOTIFIER_ITEM_INTERFACE: &str = "org.freedesktop.StatusNotifierItem";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayPixmap {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayItem {
    pub id: String,
    pub service: String,
    pub path: String,
    pub title: String,
    pub status: String,
    pub icon_name: String,
    pub overlay_icon_name: String,
    pub icon_pixmaps: Vec<TrayPixmap>,
    pub tooltip_title: String,
    pub item_is_menu: bool,
    pub menu_path: String,
    pub ordering_index: i32,
}

impl TrayItem {
    pub fn visible(&self) -> bool {
        !self.status.eq_ignore_ascii_case("passive")
    }

    pub fn label(&self) -> &str {
        if !self.title.is_empty() {
            &self.title
        } else if !self.tooltip_title.is_empty() {
            &self.tooltip_title
        } else {
            &self.id
        }
    }

    pub fn usable(&self) -> bool {
        !self.title.is_empty()
            || !self.tooltip_title.is_empty()
            || !self.icon_name.is_empty()
            || !self.overlay_icon_name.is_empty()
            || !self.icon_pixmaps.is_empty()
            || !self.menu_path.is_empty()
            || self.item_is_menu
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraySnapshot {
    pub available: bool,
    pub items: Vec<TrayItem>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Activate,
    SecondaryActivate,
    ContextMenu,
    Scroll { delta: i32, orientation: String },
}

struct TrayClient {
    connection: Connection,
}

static TRAY_CLIENT: OnceLock<Result<TrayClient, String>> = OnceLock::new();

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MprisPlayer {
    pub bus_name: String,
    pub identity: String,
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
    pub can_quit: bool,
}

impl MprisPlayer {
    pub fn key(&self) -> &str {
        if self.bus_name.is_empty() {
            &self.identity
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

pub fn list_mpris_players() -> Result<Vec<MprisPlayer>, String> {
    let connection =
        Connection::session().map_err(|error| format!("connect session bus: {error}"))?;
    let dbus = Proxy::new(&connection, DBUS_DESTINATION, DBUS_PATH, DBUS_INTERFACE)
        .map_err(|error| format!("create D-Bus proxy: {error}"))?;
    let mut names: Vec<String> = dbus
        .call("ListNames", &())
        .map_err(|error| format!("list D-Bus names: {error}"))?;
    names.retain(|name| name.starts_with(MPRIS_PREFIX));
    names.sort();

    let mut players = Vec::with_capacity(names.len());
    for name in names {
        if let Ok(player) = read_mpris_player(&connection, &name) {
            players.push(player);
        }
    }
    Ok(players)
}

pub fn call_default_player(method: &str) -> Result<(), String> {
    let players = list_mpris_players()?;
    let player = select_action_player(&players, method)
        .ok_or_else(|| "no controllable MPRIS player".to_string())?;
    call_player(player.key(), method)
}

pub fn call_player(bus_name: &str, method: &str) -> Result<(), String> {
    if !bus_name.starts_with(MPRIS_PREFIX) {
        return Err("invalid MPRIS bus name".to_string());
    }
    if !matches!(method, "PlayPause" | "Play" | "Pause" | "Next" | "Previous") {
        return Err("invalid MPRIS method".to_string());
    }
    let connection =
        Connection::session().map_err(|error| format!("connect session bus: {error}"))?;
    let proxy = Proxy::new(&connection, bus_name, MPRIS_PATH, MPRIS_PLAYER_INTERFACE)
        .map_err(|error| format!("create MPRIS proxy: {error}"))?;
    proxy
        .call::<_, _, ()>(method, &())
        .map_err(|error| format!("MPRIS {method}: {error}"))
}

pub fn select_action_player<'a>(
    players: &'a [MprisPlayer],
    method: &str,
) -> Option<&'a MprisPlayer> {
    let supports = |player: &&MprisPlayer| match method {
        "PlayPause" => player.can_toggle_playing(),
        "Play" => player.can_play || player.can_toggle_playing(),
        "Pause" => player.can_pause || player.can_toggle_playing(),
        "Next" => player.can_go_next,
        "Previous" => player.can_go_previous,
        _ => false,
    };
    if matches!(method, "PlayPause" | "Pause") {
        players
            .iter()
            .filter(supports)
            .find(|player| player.status.eq_ignore_ascii_case("playing"))
            .or_else(|| {
                players
                    .iter()
                    .filter(supports)
                    .find(|player| player.has_metadata())
            })
            .or_else(|| players.iter().filter(supports).next())
    } else {
        players
            .iter()
            .filter(supports)
            .find(|player| player.has_metadata())
            .or_else(|| players.iter().filter(supports).next())
    }
}

pub fn tray_snapshot() -> Result<TraySnapshot, String> {
    let client = tray_client()?;
    let watcher = Proxy::new(
        &client.connection,
        STATUS_NOTIFIER_WATCHER_DESTINATION,
        STATUS_NOTIFIER_WATCHER_PATH,
        STATUS_NOTIFIER_WATCHER_INTERFACE,
    )
    .map_err(|error| format!("create status notifier watcher proxy: {error}"))?;
    let mut registered: Vec<String> = watcher
        .get_property("RegisteredStatusNotifierItems")
        .map_err(|error| format!("read status notifier items: {error}"))?;
    registered.sort();
    let mut items = registered
        .iter()
        .filter_map(|spec| read_tray_item(&client.connection, spec).ok())
        .filter(|item| item.visible() && item.usable())
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        left.ordering_index
            .cmp(&right.ordering_index)
            .then_with(|| left.label().cmp(right.label()))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(TraySnapshot {
        available: true,
        items,
        error: None,
    })
}

pub fn tray_action(item: &TrayItem, action: TrayAction) -> Result<(), String> {
    let client = tray_client()?;
    let proxy = Proxy::new(
        &client.connection,
        item.service.as_str(),
        item.path.as_str(),
        STATUS_NOTIFIER_ITEM_INTERFACE,
    )
    .map_err(|error| format!("create tray item proxy: {error}"))?;
    match action {
        TrayAction::Activate => proxy
            .call::<_, _, ()>("Activate", &(0i32, 0i32))
            .map_err(|error| format!("tray activate: {error}")),
        TrayAction::SecondaryActivate => proxy
            .call::<_, _, ()>("SecondaryActivate", &(0i32, 0i32))
            .map_err(|error| format!("tray secondary activate: {error}")),
        TrayAction::ContextMenu => proxy
            .call::<_, _, ()>("ContextMenu", &(0i32, 0i32))
            .map_err(|error| format!("tray context menu: {error}")),
        TrayAction::Scroll { delta, orientation } => proxy
            .call::<_, _, ()>("Scroll", &(delta, orientation))
            .map_err(|error| format!("tray scroll: {error}")),
    }
}

fn tray_client() -> Result<&'static TrayClient, String> {
    match TRAY_CLIENT.get_or_init(|| TrayClient::connect().map_err(|error| error.to_string())) {
        Ok(client) => Ok(client),
        Err(error) => Err(error.clone()),
    }
}

impl TrayClient {
    fn connect() -> Result<Self, String> {
        let connection =
            Connection::session().map_err(|error| format!("connect session bus: {error}"))?;
        let host_name = connection
            .unique_name()
            .map(ToString::to_string)
            .ok_or_else(|| "session bus did not assign a unique name".to_string())?;
        let watcher = Proxy::new(
            &connection,
            STATUS_NOTIFIER_WATCHER_DESTINATION,
            STATUS_NOTIFIER_WATCHER_PATH,
            STATUS_NOTIFIER_WATCHER_INTERFACE,
        )
        .map_err(|error| format!("create status notifier watcher proxy: {error}"))?;
        watcher
            .call::<_, _, ()>("RegisterStatusNotifierHost", &host_name)
            .map_err(|error| format!("register status notifier host: {error}"))?;
        Ok(Self { connection })
    }
}

fn read_tray_item(connection: &Connection, spec: &str) -> Result<TrayItem, String> {
    let (service, path) = split_tray_spec(spec)?;
    let proxy = Proxy::new(
        connection,
        service.as_str(),
        path.as_str(),
        STATUS_NOTIFIER_ITEM_INTERFACE,
    )
    .map_err(|error| format!("create tray item proxy: {error}"))?;
    let id: String = property_or_default(&proxy, "Id");
    let title: String = property_or_default(&proxy, "Title");
    let status: String = property_or_default(&proxy, "Status");
    let icon_name: String = property_or_default(&proxy, "IconName");
    let overlay_icon_name: String = property_or_default(&proxy, "OverlayIconName");
    let icon_pixmaps = proxy
        .get_property::<Vec<(i32, i32, Vec<u8>)>>("IconPixmap")
        .unwrap_or_default()
        .into_iter()
        .map(|(width, height, pixels)| TrayPixmap {
            width,
            height,
            pixels,
        })
        .collect();
    let tooltip_title = proxy
        .get_property::<(String, Vec<(i32, i32, Vec<u8>)>, String, String)>("ToolTip")
        .map(|(_, _, title, _)| title)
        .unwrap_or_default();
    let menu_path = proxy
        .get_property::<OwnedObjectPath>("Menu")
        .map(|path| path.to_string())
        .unwrap_or_default();
    let ordering_index: i32 = property_or_default(&proxy, "XAyatanaOrderingIndex");
    let item_is_menu: bool = property_or_default(&proxy, "ItemIsMenu");
    drop(proxy);
    Ok(TrayItem {
        id: if id.is_empty() {
            format!("{service}{path}")
        } else {
            id
        },
        service,
        path,
        title,
        status,
        icon_name,
        overlay_icon_name,
        icon_pixmaps,
        tooltip_title,
        item_is_menu,
        menu_path,
        ordering_index,
    })
}

fn split_tray_spec(spec: &str) -> Result<(String, String), String> {
    let Some((service, path)) = spec.split_once('/') else {
        return Err("status notifier item has no object path".to_string());
    };
    if service.is_empty() || path.is_empty() {
        return Err("status notifier item has an empty service or object path".to_string());
    }
    Ok((service.to_string(), format!("/{path}")))
}

fn read_mpris_player(connection: &Connection, bus_name: &str) -> Result<MprisPlayer, String> {
    let root = Proxy::new(connection, bus_name, MPRIS_PATH, MPRIS_ROOT_INTERFACE)
        .map_err(|error| format!("create MPRIS root proxy: {error}"))?;
    let player = Proxy::new(connection, bus_name, MPRIS_PATH, MPRIS_PLAYER_INTERFACE)
        .map_err(|error| format!("create MPRIS player proxy: {error}"))?;
    let metadata = player
        .get_property::<HashMap<String, OwnedValue>>("Metadata")
        .unwrap_or_default();

    Ok(MprisPlayer {
        bus_name: bus_name.to_string(),
        identity: property_or_default(&root, "Identity"),
        desktop_entry: property_or_default(&root, "DesktopEntry"),
        status: property_or_default(&player, "PlaybackStatus"),
        artist: metadata_string_list(&metadata, "xesam:artist"),
        title: metadata_string(&metadata, "xesam:title"),
        album: metadata_string(&metadata, "xesam:album"),
        art_url: metadata_string(&metadata, "mpris:artUrl"),
        can_go_next: property_or_default(&player, "CanGoNext"),
        can_go_previous: property_or_default(&player, "CanGoPrevious"),
        can_play: property_or_default(&player, "CanPlay"),
        can_pause: property_or_default(&player, "CanPause"),
        can_quit: property_or_default(&root, "CanQuit"),
    })
}

fn property_or_default<T>(proxy: &Proxy<'_>, name: &str) -> T
where
    T: TryFrom<OwnedValue> + Default,
    T::Error: Into<zbus::Error>,
{
    proxy.get_property(name).unwrap_or_default()
}

fn metadata_string(metadata: &HashMap<String, OwnedValue>, key: &str) -> String {
    metadata
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok())
        .unwrap_or_default()
}

fn metadata_string_list(metadata: &HashMap<String, OwnedValue>, key: &str) -> String {
    metadata
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<String>::try_from(value).ok())
        .map(|values| values.join(", "))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{MprisPlayer, select_action_player};

    fn player(name: &str, status: &str, metadata: bool) -> MprisPlayer {
        MprisPlayer {
            bus_name: format!("org.mpris.MediaPlayer2.{name}"),
            identity: name.to_string(),
            status: status.to_string(),
            title: metadata.then(|| "Track".to_string()).unwrap_or_default(),
            can_go_next: true,
            can_go_previous: true,
            can_play: true,
            can_pause: true,
            ..MprisPlayer::default()
        }
    }

    #[test]
    fn action_selection_prefers_playing_player_for_pause() {
        let players = vec![
            player("idle", "Paused", true),
            player("active", "Playing", true),
        ];
        assert_eq!(
            select_action_player(&players, "Pause").unwrap().identity,
            "active"
        );
    }

    #[test]
    fn action_selection_requires_capability_for_next() {
        let mut blocked = player("blocked", "Playing", true);
        blocked.can_go_next = false;
        let available = player("available", "Paused", false);
        let players = vec![blocked, available];
        assert_eq!(
            select_action_player(&players, "Next").unwrap().identity,
            "available"
        );
    }

    #[test]
    fn player_key_uses_bus_name_for_stable_source_switching() {
        let player = player("spotify", "Playing", true);
        assert_eq!(player.key(), "org.mpris.MediaPlayer2.spotify");
    }

    #[test]
    fn tray_specs_split_service_and_object_path() {
        assert_eq!(
            super::split_tray_spec("org.example.App/StatusNotifierItem").unwrap(),
            (
                "org.example.App".to_string(),
                "/StatusNotifierItem".to_string()
            )
        );
        assert!(super::split_tray_spec("org.example.App").is_err());
    }

    #[test]
    fn passive_tray_items_are_not_visible() {
        let item = super::TrayItem {
            status: "Passive".to_string(),
            ..super::TrayItem::default()
        };
        assert!(!item.visible());
    }
}
