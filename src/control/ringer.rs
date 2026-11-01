use gtk::glib::prelude::*;

use crate::agent::dbus::{self, PropertyMap};

pub const FEEDBACKD: &str = "org.sigxcpu.Feedback";
const PATH: &str = "/org/sigxcpu/Feedback";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Vibrate,
    Silent,
}

impl Mode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "vibrate" => Some(Self::Vibrate),
            "silent" => Some(Self::Silent),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Vibrate => "vibrate",
            Self::Silent => "silent",
        }
    }

    /// feedbackd's `quiet` profile keeps haptics, so it is mapped to vibrate.
    fn profile(self) -> &'static str {
        match self {
            Self::Normal => "full",
            Self::Vibrate => "quiet",
            Self::Silent => "silent",
        }
    }

    fn from_profile(profile: &str) -> Option<Self> {
        match profile {
            "full" => Some(Self::Normal),
            "quiet" => Some(Self::Vibrate),
            "silent" => Some(Self::Silent),
            _ => None,
        }
    }
}

/// Plays a feedbackd event such as `button-pressed`; the ringer profile
/// decides whether it vibrates, sounds or does nothing.
pub async fn trigger(event: &str) {
    let Some(session) = dbus::session().await else {
        return;
    };
    let hints: std::collections::HashMap<String, gtk::glib::Variant> = Default::default();
    if let Err(err) = dbus::call(
        &session,
        FEEDBACKD,
        PATH,
        FEEDBACKD,
        "TriggerFeedback",
        Some((crate::config::APP_ID, event, hints, -1i32).to_variant()),
    )
    .await
    {
        log::debug!("Feedback {event} failed: {err}");
    }
}

pub async fn read() -> Option<Mode> {
    let session = dbus::session().await?;
    let props = dbus::properties(&session, FEEDBACKD, PATH, FEEDBACKD).await?;
    Mode::from_profile(&props.value::<String>("Profile")?)
}

pub async fn set(mode: Mode) -> Result<(), String> {
    let session = dbus::session()
        .await
        .ok_or("the session bus is not reachable")?;
    dbus::call(
        &session,
        FEEDBACKD,
        PATH,
        "org.freedesktop.DBus.Properties",
        "Set",
        Some((FEEDBACKD, "Profile", mode.profile().to_variant()).to_variant()),
    )
    .await
    .map(drop)
    .map_err(|err| err.message().to_owned())
}
