use std::collections::HashMap;

use crate::agent::dbus::{self, PropertyMap};

pub const MPRIS: &str = "org.mpris.MediaPlayer2.*";
pub const NAMESPACE: &str = "org.mpris.MediaPlayer2";
const PREFIX: &str = "org.mpris.MediaPlayer2.";
pub const PATH: &str = "/org/mpris/MediaPlayer2";
pub const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    Stop,
}

impl Action {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "play" => Some(Self::Play),
            "pause" => Some(Self::Pause),
            "play_pause" => Some(Self::PlayPause),
            "next" => Some(Self::Next),
            "previous" => Some(Self::Previous),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Play => "Play",
            Self::Pause => "Pause",
            Self::PlayPause => "PlayPause",
            Self::Next => "Next",
            Self::Previous => "Previous",
            Self::Stop => "Stop",
        }
    }
}

/// `media_package_name` is accepted as a full bus name or its last part.
pub fn bus_name(player: &str) -> String {
    if player.starts_with(PREFIX) {
        player.to_owned()
    } else {
        format!("{PREFIX}{player}")
    }
}

async fn players(session: &gtk::gio::DBusConnection) -> Vec<String> {
    dbus::names(session)
        .await
        .into_iter()
        .filter(|name| name.starts_with(PREFIX))
        .collect()
}

#[derive(Debug, Default, PartialEq)]
pub struct Session {
    /// `playing`, `paused` or `stopped`, or `none` without a player.
    pub state: String,
    pub player: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// The playing player, else a paused one, else the first.
pub async fn session() -> Option<Session> {
    let session = dbus::session().await?;
    let mut best: Option<(u8, Session)> = None;
    for name in players(&session).await {
        let Some(props) = dbus::properties(&session, &name, PATH, PLAYER).await else {
            continue;
        };
        let status = props
            .value::<String>("PlaybackStatus")
            .unwrap_or_default()
            .to_lowercase();
        let rank = match status.as_str() {
            "playing" => 0,
            "paused" => 1,
            _ => 2,
        };
        if best.as_ref().is_some_and(|(best, _)| *best <= rank) {
            continue;
        }
        let metadata = props
            .get("Metadata")
            .and_then(|value| value.get::<HashMap<String, gtk::glib::Variant>>())
            .unwrap_or_default();
        let text = |key: &str| metadata.get(key).and_then(|value| value.get::<String>());
        let artist = metadata
            .get("xesam:artist")
            .and_then(|value| value.get::<Vec<String>>())
            .map(|artists| artists.join(", "))
            .filter(|artists| !artists.is_empty());
        best = Some((
            rank,
            Session {
                state: if status.is_empty() {
                    "stopped".into()
                } else {
                    status
                },
                player: Some(name.trim_start_matches(PREFIX).to_owned()),
                title: text("xesam:title"),
                artist,
                album: text("xesam:album"),
            },
        ));
    }
    Some(best.map(|(_, session)| session).unwrap_or(Session {
        state: "none".into(),
        ..Session::default()
    }))
}

pub async fn run(action: Action, player: Option<&str>) -> Result<(), String> {
    let session = dbus::session()
        .await
        .ok_or("the session bus is not reachable")?;
    let players = players(&session).await;
    let target = match player {
        Some(player) => {
            let wanted = bus_name(player);
            players
                .into_iter()
                .find(|name| *name == wanted || name.starts_with(&format!("{wanted}.")))
        }
        None => {
            let mut playing = None;
            for name in &players {
                let status = dbus::properties(&session, name, PATH, PLAYER)
                    .await
                    .and_then(|props| props.value::<String>("PlaybackStatus"));
                if status.as_deref() == Some("Playing") {
                    playing = Some(name.clone());
                    break;
                }
            }
            playing.or_else(|| players.into_iter().next())
        }
    }
    .ok_or("no media player is running")?;
    dbus::call(&session, &target, PATH, PLAYER, action.method(), None)
        .await
        .map(drop)
        .map_err(|err| err.message().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_players() {
        assert_eq!(bus_name("mpv"), "org.mpris.MediaPlayer2.mpv");
        assert_eq!(
            bus_name("org.mpris.MediaPlayer2.spotify"),
            "org.mpris.MediaPlayer2.spotify"
        );
        assert_eq!(Action::parse("play_pause"), Some(Action::PlayPause));
        assert_eq!(Action::parse("rewind"), None);
    }
}
