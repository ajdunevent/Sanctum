//! Command names follow the Android companion app where an equivalent exists.
//! Levels are percentages where Android uses device-specific steps.

pub mod audio;
pub mod bluetooth;
pub mod catalog;
pub mod display;
pub mod media;
pub mod ringer;
pub mod sandbox;
pub mod settings;

use gtk::gio::prelude::*;
use serde_json::Value;

use crate::notify::Click;
use catalog::Kind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    On,
    Off,
    Toggle,
}

impl Switch {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value? {
            "turn_on" | "on" => Some(Self::On),
            "turn_off" | "off" => Some(Self::Off),
            "toggle" => Some(Self::Toggle),
            _ => None,
        }
    }

    pub fn resolve(self, current: bool) -> bool {
        match self {
            Self::On => true,
            Self::Off => false,
            Self::Toggle => !current,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Volume(u32),
    Mute(Switch),
    RingerMode(ringer::Mode),
    DoNotDisturb(bool),
    Media {
        action: media::Action,
        player: Option<String>,
    },
    ScreenOn,
    ScreenOff,
    Lock,
    Brightness(u32),
    ScreenTimeout(u32),
    Flashlight(Switch),
    Bluetooth(Switch),
    UpdateSensors,
    StopTts,
    Open(Click),
    Unsupported {
        command: String,
        reason: String,
    },
}

impl Command {
    pub fn kind(&self) -> Option<Kind> {
        Some(match self {
            Self::Volume(_) | Self::Mute(_) => Kind::Volume,
            Self::RingerMode(_) => Kind::RingerMode,
            Self::DoNotDisturb(_) => Kind::DoNotDisturb,
            Self::Media { .. } => Kind::Media,
            Self::ScreenOn => Kind::ScreenOn,
            Self::ScreenOff => Kind::ScreenOff,
            Self::Lock => Kind::Lock,
            Self::Brightness(_) => Kind::Brightness,
            Self::ScreenTimeout(_) => Kind::ScreenTimeout,
            Self::Flashlight(_) => Kind::Flashlight,
            Self::Bluetooth(_) => Kind::Bluetooth,
            Self::UpdateSensors => Kind::UpdateSensors,
            Self::StopTts => Kind::StopTts,
            Self::Open(_) => Kind::Open,
            Self::Unsupported { .. } => return None,
        })
    }
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_owned()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn percent(value: Option<&str>) -> Option<u32> {
    let value: f64 = value?.parse().ok()?;
    value
        .is_finite()
        .then(|| value.round().clamp(0.0, 100.0) as u32)
}

pub fn parse(message: &str, data: &Value) -> Option<Command> {
    if !message.starts_with("command_") {
        return None;
    }
    let argument = text(&data["command"]);
    let argument = argument.as_deref();
    let unsupported = |reason: &str| Command::Unsupported {
        command: message.to_owned(),
        reason: reason.to_owned(),
    };
    let bad_argument = || unsupported("the command value is missing or not understood");
    Some(match message {
        "command_volume_level" => {
            let stream = data["media_stream"].as_str().unwrap_or("music_stream");
            if stream != "music_stream" {
                unsupported("only music_stream is available; it sets the output volume")
            } else if let Some(switch) = argument
                .and_then(|value| value.strip_suffix("mute"))
                .and_then(|prefix| match prefix {
                    "" => Some(Switch::On),
                    "un" => Some(Switch::Off),
                    "toggle_" => Some(Switch::Toggle),
                    _ => None,
                })
            {
                Command::Mute(switch)
            } else {
                percent(argument).map_or_else(bad_argument, Command::Volume)
            }
        }
        "command_screen_on" => Command::ScreenOn,
        "command_screen_off" => Command::ScreenOff,
        "command_lock" => Command::Lock,
        "command_screen_brightness_level" => {
            percent(argument).map_or_else(bad_argument, Command::Brightness)
        }
        "command_ringer_mode" => argument
            .and_then(ringer::Mode::parse)
            .map_or_else(bad_argument, Command::RingerMode),
        "command_dnd" => match argument {
            Some("off") => Command::DoNotDisturb(false),
            Some("priority_only" | "alarms_only" | "total_silence") => Command::DoNotDisturb(true),
            _ => bad_argument(),
        },
        "command_media" => match data["media_command"]
            .as_str()
            .and_then(media::Action::parse)
        {
            Some(action) => Command::Media {
                action,
                player: text(&data["media_package_name"]).filter(|name| !name.is_empty()),
            },
            None => bad_argument(),
        },
        "command_screen_off_timeout" => match argument.and_then(|ms| ms.parse::<u64>().ok()) {
            Some(ms) => Command::ScreenTimeout(ms.div_ceil(1000).min(u64::from(u32::MAX)) as u32),
            None => bad_argument(),
        },
        "command_bluetooth" => {
            Switch::parse(argument).map_or_else(bad_argument, Command::Bluetooth)
        }
        "command_flashlight" => {
            Switch::parse(argument).map_or_else(bad_argument, Command::Flashlight)
        }
        "command_update_sensors" => Command::UpdateSensors,
        "command_stop_tts" => Command::StopTts,
        "command_webview" => Command::Open(Click::parse(argument)),
        _ => unsupported("this command is not known to Sanctum"),
    })
}

/// Whether the user allows this command, from the switches on the Controls
/// page.
pub fn allowed(settings: &gtk::gio::Settings, command: &Command) -> bool {
    settings.boolean("device-commands")
        && command.kind().is_none_or(|kind| {
            !settings
                .strv("disabled-commands")
                .iter()
                .any(|id| id.as_str() == kind.message())
        })
}

pub async fn run(app: &crate::application::Application, command: Command) {
    let result = match &command {
        Command::Volume(level) => audio::set_volume(*level).await,
        Command::Mute(switch) => audio::set_muted(*switch).await,
        Command::RingerMode(mode) => ringer::set(*mode).await,
        Command::DoNotDisturb(on) => settings::set_banners_hidden(*on).await,
        Command::Media { action, player } => media::run(*action, player.as_deref()).await,
        Command::ScreenTimeout(seconds) => settings::set_screen_timeout(*seconds).await,
        Command::Bluetooth(switch) => bluetooth::set(*switch).await,
        Command::ScreenOn => display::set_screen_on(true).await,
        Command::ScreenOff => display::set_screen_on(false).await,
        Command::Lock => display::lock().await,
        Command::Brightness(level) => display::set_brightness(*level).await,
        Command::Flashlight(switch) => display::set_flashlight(*switch).await,
        Command::UpdateSensors => {
            app.agent().refresh_all();
            Ok(())
        }
        Command::StopTts => {
            crate::voice::stop_playback();
            Ok(())
        }
        Command::Open(click) => {
            app.activate_action("open-click", Some(&click.target().to_variant()));
            Ok(())
        }
        Command::Unsupported { command, reason } => Err(format!("{command}: {reason}")),
    };
    match result {
        Ok(()) => {
            log::info!("Ran device command {command:?}");
            app.agent().refresh_controls();
        }
        Err(err) => log::warn!("Device command failed: {err}"),
    }
}

/// Output is requested in the C locale so it can be parsed.
/// How long a helper such as `pactl` may take before it is stopped.
const OUTPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Helpers are stopped when Sanctum exits, however it exits. Without this a
/// helper outlives a quit or a kill and keeps running on its own.
pub(crate) fn launcher(flags: gtk::gio::SubprocessFlags) -> gtk::gio::SubprocessLauncher {
    let launcher = gtk::gio::SubprocessLauncher::new(flags);
    launcher.setenv("LC_ALL", "C", true);
    launcher.set_child_setup(|| {
        // SAFETY: runs in the forked child before exec; prctl is
        // async-signal-safe.
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
        }
    });
    launcher
}

pub(crate) async fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let launcher =
        launcher(gtk::gio::SubprocessFlags::STDOUT_PIPE | gtk::gio::SubprocessFlags::STDERR_PIPE);
    let mut argv: Vec<&std::ffi::OsStr> = vec![program.as_ref()];
    argv.extend(args.iter().map(std::ffi::OsStr::new));
    let process = launcher
        .spawn(&argv)
        .map_err(|err| format!("{program}: {err}"))?;
    let Ok(result) =
        gtk::glib::future_with_timeout(OUTPUT_TIMEOUT, process.communicate_utf8_future(None)).await
    else {
        process.force_exit();
        return Err(format!("{program}: no answer"));
    };
    let (stdout, stderr) = result.map_err(|err| format!("{program}: {err}"))?;
    if process.is_successful() {
        Ok(stdout.map(|text| text.to_string()).unwrap_or_default())
    } else {
        let stderr = stderr
            .map(|text| text.trim().to_owned())
            .unwrap_or_default();
        Err(format!("{program}: {stderr}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command(message: &str, data: Value) -> Option<Command> {
        parse(message, &data)
    }

    #[test]
    fn parses_commands() {
        assert_eq!(command("hello", json!({})), None);
        assert_eq!(
            command("command_volume_level", json!({"command": 40})),
            Some(Command::Volume(40))
        );
        assert_eq!(
            command(
                "command_volume_level",
                json!({"command": "140", "media_stream": "music_stream"})
            ),
            Some(Command::Volume(100))
        );
        assert_eq!(
            command("command_volume_level", json!({"command": "toggle_mute"})),
            Some(Command::Mute(Switch::Toggle))
        );
        assert!(matches!(
            command(
                "command_volume_level",
                json!({"command": 3, "media_stream": "ring_stream"})
            ),
            Some(Command::Unsupported { .. })
        ));
        assert_eq!(
            command("command_flashlight", json!({"command": "turn_on"})),
            Some(Command::Flashlight(Switch::On))
        );
        assert!(matches!(
            command("command_flashlight", json!({})),
            Some(Command::Unsupported { .. })
        ));
        assert_eq!(
            command("command_webview", json!({"command": "entityId:light.desk"})),
            Some(Command::Open(Click::Entity("light.desk".into())))
        );
        assert_eq!(
            command("command_dnd", json!({"command": "priority_only"})),
            Some(Command::DoNotDisturb(true))
        );
        assert_eq!(
            command("command_ringer_mode", json!({"command": "vibrate"})),
            Some(Command::RingerMode(ringer::Mode::Vibrate))
        );
        assert_eq!(
            command("command_screen_off_timeout", json!({"command": 30001})),
            Some(Command::ScreenTimeout(31))
        );
        assert_eq!(
            command(
                "command_media",
                json!({"media_command": "next", "media_package_name": "mpv"})
            ),
            Some(Command::Media {
                action: media::Action::Next,
                player: Some("mpv".into())
            })
        );
        assert!(matches!(
            command("command_launch_app", json!({})),
            Some(Command::Unsupported { .. })
        ));
    }
}
