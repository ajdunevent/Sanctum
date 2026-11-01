//! Binding the FuriOS assistant button to Assist. The button daemon re-reads
//! `~/.config/assistant-button/` on every press: a custom command file per
//! gesture overrides that gesture's predefined action, which GNOME Settings
//! owns. Sanctum writes or removes only the custom command file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gettextrs::gettext;
use gtk::gio;
use gtk::glib;

use crate::agent::dbus;
use crate::config::APP_ID;

pub const SERVICE: &str = "io.FuriOS.AssistantButton";
const OBJECT_PATH: &str = "/io/FuriOS/AssistantButton";
/// `ActionPerformed` reports this action index when a custom command ran.
pub const CUSTOM_ACTION: i32 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Short,
    Double,
    Long,
}

impl Gesture {
    pub const ALL: [Self; 3] = [Self::Short, Self::Double, Self::Long];

    fn file(self) -> &'static str {
        match self {
            Self::Short => "short_press",
            Self::Double => "double_press",
            Self::Long => "long_press",
        }
    }

    /// The event number in `ActionPerformed`.
    pub fn event(self) -> i32 {
        match self {
            Self::Short => 1,
            Self::Long => 2,
            Self::Double => 3,
        }
    }

    pub fn title(self) -> String {
        match self {
            Self::Short => gettext("Short Press"),
            Self::Double => gettext("Double Press"),
            Self::Long => gettext("Long Press"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// One of GNOME Settings' built-in actions, numbered 1 to 8.
    Predefined(u32),
    /// The command Sanctum writes.
    Assist,
    Custom(String),
}

impl Action {
    pub fn is_set(&self) -> bool {
        *self != Self::Nothing
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Nothing => gettext("Nothing"),
            Self::Assist => gettext("Open Sanctum Assist"),
            Self::Custom(command) => gettext("Custom command: %s").replacen("%s", command, 1),
            Self::Predefined(1) => gettext("Flashlight"),
            Self::Predefined(2) => gettext("Open camera"),
            Self::Predefined(3) => gettext("Take a picture"),
            Self::Predefined(4) => gettext("Screenshot"),
            Self::Predefined(5) => gettext("Tab key"),
            Self::Predefined(6) => gettext("Rotate screen"),
            Self::Predefined(7) => gettext("Back"),
            Self::Predefined(8) => gettext("Escape key"),
            Self::Predefined(other) => gettext("Action %d").replacen("%d", &other.to_string(), 1),
        }
    }
}

pub async fn is_available() -> bool {
    match dbus::session().await {
        Some(bus) => dbus::has_owner(&bus, SERVICE).await,
        None => false,
    }
}

/// The host's config directory. Inside Flatpak the app's config home is
/// redirected, but the `xdg-config/assistant-button` permission mounts the
/// host directory at its real path.
pub fn directory() -> PathBuf {
    let config = std::env::var_os("HOST_XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| {
            if Path::new("/.flatpak-info").exists() {
                glib::home_dir().join(".config")
            } else {
                glib::user_config_dir()
            }
        });
    config.join("assistant-button")
}

/// The command written for Assist, listening at once when `voice` is set.
/// `gapplication action` reaches a running Sanctum over D-Bus without
/// starting a sandbox, and D-Bus activates it otherwise; the fallback covers
/// systems without `gapplication`.
pub fn command(voice: bool) -> String {
    let (action, flags) = if voice {
        ("assist-voice", "--assist --voice")
    } else {
        ("assist", "--assist")
    };
    let launch = if Path::new("/.flatpak-info").exists() {
        format!("flatpak run {APP_ID} {flags}")
    } else {
        format!("sanctum {flags}")
    };
    format!("gapplication action {APP_ID} {action} 2>/dev/null || {launch}")
}

fn is_ours(command: &str) -> bool {
    command == self::command(false) || command == self::command(true)
}

fn read(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

pub fn current(directory: &Path, gesture: Gesture) -> Action {
    if let Some(command) = read(&directory.join(gesture.file())) {
        return if is_ours(&command) {
            Action::Assist
        } else {
            Action::Custom(command)
        };
    }
    match read(&directory.join(format!("{}_predefined", gesture.file())))
        .and_then(|value| value.parse::<u32>().ok())
    {
        Some(0) | None => Action::Nothing,
        Some(value) => Action::Predefined(value),
    }
}

/// Writes the custom command file through a temporary file and a rename, so
/// a press during the write never runs half a command.
pub fn bind(directory: &Path, gesture: Gesture, voice: bool) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let target = directory.join(gesture.file());
    let temporary = directory.join(format!(".{}.sanctum", gesture.file()));
    fs::write(&temporary, format!("{}\n", command(voice)))?;
    fs::rename(&temporary, &target)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Unbound {
    Removed,
    /// The file no longer holds Sanctum's command, so it was left alone.
    ChangedElsewhere,
    NotBound,
}

pub fn unbind(directory: &Path, gesture: Gesture) -> io::Result<Unbound> {
    let path = directory.join(gesture.file());
    match read(&path) {
        None => Ok(Unbound::NotBound),
        Some(text) if is_ours(&text) => {
            fs::remove_file(&path)?;
            Ok(Unbound::Removed)
        }
        Some(_) => Ok(Unbound::ChangedElsewhere),
    }
}

/// Calls `f` with the action index and event number of every press.
pub fn watch_presses(
    bus: &gio::DBusConnection,
    f: impl Fn(i32, i32) + 'static,
) -> gio::SignalSubscription {
    bus.subscribe_to_signal(
        Some(SERVICE),
        None,
        Some("ActionPerformed"),
        Some(OBJECT_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            if let Some((action, event)) = signal.parameters.get::<(i32, i32)>() {
                f(action, event);
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sanctum-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn custom_file_overrides_predefined() {
        let dir = scratch("override");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(current(&dir, Gesture::Short), Action::Nothing);
        fs::write(dir.join("short_press_predefined"), "1\n").unwrap();
        assert_eq!(current(&dir, Gesture::Short), Action::Predefined(1));
        fs::write(dir.join("short_press"), "notify-send hi").unwrap();
        assert_eq!(
            current(&dir, Gesture::Short),
            Action::Custom("notify-send hi".into())
        );
        fs::write(dir.join("long_press_predefined"), "0").unwrap();
        assert_eq!(current(&dir, Gesture::Long), Action::Nothing);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bind_and_unbind() {
        let dir = scratch("bind");
        bind(&dir, Gesture::Double, false).unwrap();
        assert_eq!(current(&dir, Gesture::Double), Action::Assist);
        bind(&dir, Gesture::Double, true).unwrap();
        assert_eq!(current(&dir, Gesture::Double), Action::Assist);
        assert!(!dir.join(".double_press.sanctum").exists());
        assert_eq!(unbind(&dir, Gesture::Double).unwrap(), Unbound::Removed);
        assert_eq!(unbind(&dir, Gesture::Double).unwrap(), Unbound::NotBound);
        bind(&dir, Gesture::Long, true).unwrap();
        fs::write(dir.join("long_press"), "something else").unwrap();
        assert_eq!(
            unbind(&dir, Gesture::Long).unwrap(),
            Unbound::ChangedElsewhere
        );
        assert!(dir.join("long_press").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
