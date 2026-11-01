use gettextrs::gettext;

use super::sandbox::{Permission, Policy};
use super::{audio, bluetooth, display, media, ringer, settings};
use crate::agent::dbus;
use crate::agent::system::{DISPLAY_CONFIG, LOGIND};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Sound,
    Screen,
    Hardware,
    App,
}

impl Group {
    pub const ALL: [Self; 4] = [Self::Sound, Self::Screen, Self::Hardware, Self::App];

    pub fn title(self) -> String {
        match self {
            Self::Sound => gettext("Sound and Notifications"),
            Self::Screen => gettext("Screen"),
            Self::Hardware => gettext("Flashlight and Bluetooth"),
            Self::App => gettext("Sanctum"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Volume,
    RingerMode,
    DoNotDisturb,
    Media,
    ScreenOn,
    ScreenOff,
    Lock,
    Brightness,
    ScreenTimeout,
    Flashlight,
    Bluetooth,
    UpdateSensors,
    StopTts,
    Open,
}

impl Kind {
    pub const ALL: [Self; 14] = [
        Self::Volume,
        Self::RingerMode,
        Self::DoNotDisturb,
        Self::Media,
        Self::ScreenOn,
        Self::ScreenOff,
        Self::Lock,
        Self::Brightness,
        Self::ScreenTimeout,
        Self::Flashlight,
        Self::Bluetooth,
        Self::UpdateSensors,
        Self::StopTts,
        Self::Open,
    ];

    /// The message name doubles as the ID stored in `disabled-commands`.
    pub fn message(self) -> &'static str {
        match self {
            Self::Volume => "command_volume_level",
            Self::RingerMode => "command_ringer_mode",
            Self::DoNotDisturb => "command_dnd",
            Self::Media => "command_media",
            Self::ScreenOn => "command_screen_on",
            Self::ScreenOff => "command_screen_off",
            Self::Lock => "command_lock",
            Self::Brightness => "command_screen_brightness_level",
            Self::ScreenTimeout => "command_screen_off_timeout",
            Self::Flashlight => "command_flashlight",
            Self::Bluetooth => "command_bluetooth",
            Self::UpdateSensors => "command_update_sensors",
            Self::StopTts => "command_stop_tts",
            Self::Open => "command_webview",
        }
    }

    pub fn group(self) -> Group {
        match self {
            Self::Volume | Self::RingerMode | Self::DoNotDisturb | Self::Media => Group::Sound,
            Self::ScreenOn
            | Self::ScreenOff
            | Self::Lock
            | Self::Brightness
            | Self::ScreenTimeout => Group::Screen,
            Self::Flashlight | Self::Bluetooth => Group::Hardware,
            Self::UpdateSensors | Self::StopTts | Self::Open => Group::App,
        }
    }

    pub fn title(self) -> String {
        match self {
            Self::Volume => gettext("Volume and Mute"),
            Self::RingerMode => gettext("Ringer Mode"),
            Self::DoNotDisturb => gettext("Do Not Disturb"),
            Self::Media => gettext("Media Playback"),
            Self::ScreenOn => gettext("Wake Screen"),
            Self::ScreenOff => gettext("Turn Screen Off"),
            Self::Lock => gettext("Lock"),
            Self::Brightness => gettext("Brightness"),
            Self::ScreenTimeout => gettext("Screen Timeout"),
            Self::Flashlight => gettext("Flashlight"),
            Self::Bluetooth => gettext("Bluetooth"),
            Self::UpdateSensors => gettext("Update Sensors"),
            Self::StopTts => gettext("Stop Spoken Reply"),
            Self::Open => gettext("Open Entity or Page"),
        }
    }

    pub fn permission(self) -> Option<Permission> {
        Some(match self {
            Self::Volume => Permission::Socket("pulseaudio"),
            Self::RingerMode => Permission::SessionTalk(ringer::FEEDBACKD),
            Self::DoNotDisturb | Self::ScreenTimeout => Permission::SessionTalk(settings::DCONF),
            Self::Media => Permission::SessionTalk(media::MPRIS),
            Self::ScreenOn | Self::ScreenOff => Permission::SessionTalk(DISPLAY_CONFIG),
            Self::Lock | Self::Brightness | Self::Flashlight => Permission::SystemTalk(LOGIND),
            Self::Bluetooth => Permission::SystemTalk(bluetooth::BLUEZ),
            Self::UpdateSensors | Self::StopTts | Self::Open => return None,
        })
    }

    fn example_data(self) -> &'static str {
        match self {
            Self::Volume => "    command: 40\n",
            Self::RingerMode => "    command: vibrate\n",
            Self::DoNotDisturb => "    command: priority_only\n",
            Self::Media => "    media_command: play_pause\n",
            Self::Brightness => "    command: 60\n",
            // Android takes milliseconds.
            Self::ScreenTimeout => "    command: 60000\n",
            Self::Flashlight | Self::Bluetooth => "    command: toggle\n",
            Self::Open => "    command: entityId:light.kitchen\n",
            Self::ScreenOn | Self::ScreenOff | Self::Lock | Self::UpdateSensors | Self::StopTts => {
                ""
            }
        }
    }

    pub fn example(self, service: &str) -> String {
        let data = self.example_data();
        let mut yaml = format!(
            "action: notify.{service}\ndata:\n  message: {}\n",
            self.message()
        );
        if !data.is_empty() {
            yaml.push_str("  data:\n");
            yaml.push_str(data);
        }
        yaml
    }

    pub async fn availability(self) -> Availability {
        if let Some(permission) = self.permission()
            && !Policy::current().allows(permission)
        {
            return Availability::NeedsPermission(permission);
        }
        let (Some(system), Some(session)) = (dbus::system().await, dbus::session().await) else {
            return Availability::Unavailable;
        };
        let available = match self {
            Self::Volume => audio::read().await.is_some(),
            Self::RingerMode => ringer::read().await.is_some(),
            Self::DoNotDisturb | Self::ScreenTimeout => {
                dbus::service_exists(&session, settings::DCONF).await
            }
            Self::ScreenOn | Self::ScreenOff => dbus::has_owner(&session, DISPLAY_CONFIG).await,
            Self::Lock => dbus::has_owner(&system, LOGIND).await,
            Self::Brightness => {
                if !dbus::has_owner(&system, LOGIND).await {
                    false
                } else if display::backlight().is_none() {
                    return Availability::Missing(gettext("No adjustable backlight found"));
                } else {
                    true
                }
            }
            Self::Flashlight => match display::flashlight().await {
                Some(display::Torch::Flashlightd { .. }) => true,
                Some(display::Torch::Sysfs(_)) => dbus::has_owner(&system, LOGIND).await,
                None => return Availability::Missing(gettext("No flashlight found")),
            },
            Self::Bluetooth => {
                if !dbus::has_owner(&system, bluetooth::BLUEZ).await {
                    false
                } else if bluetooth::adapter().await.is_none() {
                    return Availability::Missing(gettext("No Bluetooth adapter found"));
                } else {
                    true
                }
            }
            Self::Media | Self::UpdateSensors | Self::StopTts | Self::Open => true,
        };
        if available {
            Availability::Available
        } else {
            Availability::Unavailable
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Availability {
    Available,
    Missing(String),
    NeedsPermission(Permission),
    Unavailable,
}

pub fn reaches(permission: Permission) -> String {
    match permission {
        Permission::Socket(_) => gettext("the sound server"),
        Permission::SessionTalk(ringer::FEEDBACKD) => {
            gettext("feedbackd, which keeps the ringer profile")
        }
        Permission::SessionTalk(settings::DCONF) => gettext("dconf, which stores desktop settings"),
        Permission::SessionTalk(media::MPRIS) => gettext("media players"),
        Permission::SessionTalk(DISPLAY_CONFIG) => gettext("the compositor's display settings"),
        Permission::SystemTalk(bluetooth::BLUEZ) => gettext("BlueZ, the Bluetooth service"),
        Permission::SystemTalk(LOGIND) => gettext("logind, which manages the session"),
        Permission::Filesystem(_) => gettext("the desktop settings database"),
        Permission::SessionTalk(name) | Permission::SystemTalk(name) => name.to_owned(),
    }
}

pub fn flatseal_hint(permission: Permission) -> String {
    match permission {
        Permission::Socket(_) => gettext("turn on “PulseAudio sound server” under Socket"),
        Permission::SessionTalk(name) => {
            gettext("add %s under Session Bus, Talks").replacen("%s", name, 1)
        }
        Permission::SystemTalk(name) => {
            gettext("add %s under System Bus, Talks").replacen("%s", name, 1)
        }
        Permission::Filesystem(path) => {
            gettext("add %s under Filesystem, Other files").replacen("%s", path, 1)
        }
    }
}

/// The notify action is named from the device name sent at registration.
pub fn notify_service(device_name: &str) -> String {
    let mut slug = String::new();
    for c in device_name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    format!("mobile_app_{}", slug.trim_matches('_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_unique() {
        let mut messages: Vec<_> = Kind::ALL.iter().map(|kind| kind.message()).collect();
        messages.sort_unstable();
        messages.dedup();
        assert_eq!(messages.len(), Kind::ALL.len());
    }

    #[test]
    fn builds_examples() {
        assert_eq!(
            notify_service("FuriPhone FLX1 (AJ)"),
            "mobile_app_furiphone_flx1_aj"
        );
        assert_eq!(
            Kind::Flashlight.example("mobile_app_phone"),
            "action: notify.mobile_app_phone\ndata:\n  message: command_flashlight\n  data:\n    command: toggle\n"
        );
        assert_eq!(
            Kind::Lock.example("mobile_app_phone"),
            "action: notify.mobile_app_phone\ndata:\n  message: command_lock\n"
        );
    }
}
