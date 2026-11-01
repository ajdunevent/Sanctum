//! Volume is read and set with `pactl` over the PulseAudio socket that voice
//! uses.

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

use super::{Switch, output};

const PACTL: &str = "pactl";
const SINK: &str = "@DEFAULT_SINK@";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Volume {
    pub level: u32,
    pub muted: bool,
}

pub async fn read() -> Option<Volume> {
    let volume = output(PACTL, &["get-sink-volume", SINK]).await.ok()?;
    let mute = output(PACTL, &["get-sink-mute", SINK]).await.ok()?;
    Some(Volume {
        level: parse_level(&volume)?,
        muted: parse_mute(&mute)?,
    })
}

fn parse_level(text: &str) -> Option<u32> {
    let percent = text.split('%').next()?;
    percent
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn parse_mute(text: &str) -> Option<bool> {
    match text.trim().strip_prefix("Mute:")?.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

pub async fn set_volume(level: u32) -> Result<(), String> {
    output(PACTL, &["set-sink-volume", SINK, &format!("{level}%")]).await?;
    // Setting a level also unmutes so the change is heard.
    output(PACTL, &["set-sink-mute", SINK, "0"]).await.map(drop)
}

pub async fn set_muted(switch: Switch) -> Result<(), String> {
    let value = match switch {
        Switch::On => "1",
        Switch::Off => "0",
        Switch::Toggle => "toggle",
    };
    output(PACTL, &["set-sink-mute", SINK, value])
        .await
        .map(drop)
}

pub fn watch(f: impl Fn() + 'static) -> Option<gio::Subprocess> {
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDOUT_PIPE);
    launcher.setenv("LC_ALL", "C", true);
    let process = launcher
        .spawn(&[PACTL.as_ref(), "subscribe".as_ref()])
        .ok()?;
    let stdout = process.stdout_pipe()?;
    let lines = gio::DataInputStream::new(&stdout);
    glib::spawn_future_local(async move {
        while let Ok(Some(line)) = lines.read_line_utf8_future(glib::Priority::DEFAULT).await {
            if is_output_event(&line) {
                f();
            }
        }
    });
    Some(process)
}

/// Stream events such as `on sink-input #12` fire constantly during playback
/// and are ignored.
fn is_output_event(line: &str) -> bool {
    line.contains(" on sink #") || line.contains(" on server")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pactl() {
        let volume = "Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: 26214 /  40% / -23.88 dB\n        balance 0.00\n";
        assert_eq!(parse_level(volume), Some(40));
        assert_eq!(parse_mute("Mute: yes\n"), Some(true));
        assert_eq!(parse_mute("Mute: no"), Some(false));
        assert!(is_output_event("Event 'change' on sink #53"));
        assert!(!is_output_event("Event 'change' on sink-input #12"));
    }
}
