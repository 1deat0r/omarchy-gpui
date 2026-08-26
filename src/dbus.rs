//! Small blocking D-Bus adapters used by the shell services.
//!
//! Omarchy's reference media service is backed by Quickshell's MPRIS model,
//! not by a mandatory `playerctl` executable.  This module keeps the GPUI
//! boundary equivalent: discover session-bus players, read their standard
//! MPRIS properties, and invoke only the methods exposed by each player.

use std::collections::HashMap;

use zbus::{
    blocking::{Connection, Proxy},
    zvariant::OwnedValue,
};

const DBUS_DESTINATION: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const MPRIS_PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

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
}
