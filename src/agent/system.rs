//! Session lock and screen state from logind and the compositor, and CPU,
//! memory and boot time from /proc.

use std::cell::Cell;
use std::fs;

use gtk::gio;
use gtk::glib;

use super::dbus::{self, PropertyMap};
use super::sensor::Reading;

pub const LOGIND: &str = "org.freedesktop.login1";
pub const LOGIND_SESSION: &str = "org.freedesktop.login1.Session";
/// logind resolves this path to the caller's session.
pub const OWN_SESSION: &str = "/org/freedesktop/login1/session/auto";
/// Implemented by both Mutter and Phosh.
pub const DISPLAY_CONFIG: &str = "org.gnome.Mutter.DisplayConfig";
pub const DISPLAY_CONFIG_PATH: &str = "/org/gnome/Mutter/DisplayConfig";

pub async fn read_session(
    system: Option<&gio::DBusConnection>,
    session: Option<&gio::DBusConnection>,
) -> Vec<Reading> {
    let mut readings = Vec::new();
    if let Some(system) = system
        && let Some(props) = dbus::properties(system, LOGIND, OWN_SESSION, LOGIND_SESSION).await
        && let Some(locked) = props.value::<bool>("LockedHint")
    {
        readings.push(Reading::new("session_locked", locked));
    }
    if let Some(screen) = screen_on(session).await {
        readings.push(Reading::new("screen_on", screen));
    }
    readings
}

/// `None` when the compositor does not expose its power save mode.
pub async fn screen_on(session: Option<&gio::DBusConnection>) -> Option<bool> {
    let props = dbus::properties(
        session?,
        DISPLAY_CONFIG,
        DISPLAY_CONFIG_PATH,
        DISPLAY_CONFIG,
    )
    .await?;
    // 0 is on; 1 to 3 are standby, suspend and off; -1 is unknown.
    match props.value::<i32>("PowerSaveMode")? {
        -1 => None,
        mode => Some(mode == 0),
    }
}

/// CPU usage needs two samples, so the previous counters are kept.
#[derive(Default)]
pub struct CpuSampler {
    previous: Cell<Option<(u64, u64)>>,
}

impl CpuSampler {
    /// Percent busy since the last call; `None` on the first call.
    pub fn sample(&self) -> Option<f64> {
        let stat = fs::read_to_string("/proc/stat").ok()?;
        let (busy, total) = cpu_counters(&stat)?;
        let previous = self.previous.replace(Some((busy, total)));
        let (last_busy, last_total) = previous?;
        let elapsed = total.checked_sub(last_total).filter(|delta| *delta > 0)?;
        let worked = busy.saturating_sub(last_busy);
        Some((worked as f64 / elapsed as f64 * 1000.0).round() / 10.0)
    }
}

/// Busy and total jiffies from the aggregate `cpu` line of /proc/stat.
fn cpu_counters(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    let total: u64 = values.iter().take(8).sum();
    // Fields 3 and 4 are idle and iowait.
    let idle = values.get(3)? + values.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

fn memory_usage(meminfo: &str) -> Option<f64> {
    let field = |name: &str| -> Option<f64> {
        meminfo
            .lines()
            .find(|line| line.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let total = field("MemTotal:")?;
    let available = field("MemAvailable:")?;
    Some(((total - available) / total * 1000.0).round() / 10.0)
}

fn last_boot(stat: &str) -> Option<String> {
    let seconds: i64 = stat
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    let time = glib::DateTime::from_unix_utc(seconds).ok()?;
    time.format_iso8601().ok().map(|text| text.to_string())
}

pub fn read_polled(cpu: &CpuSampler) -> Vec<Reading> {
    let mut readings = Vec::new();
    if let Some(usage) = cpu.sample() {
        readings.push(Reading::new("cpu_usage", usage));
    }
    if let Some(usage) = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| memory_usage(&meminfo))
    {
        readings.push(Reading::new("memory_usage", usage));
    }
    if let Some(boot) = fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|stat| last_boot(&stat))
    {
        readings.push(Reading::new("last_boot", boot));
    }
    readings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc() {
        let stat = "cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3 4\nbtime 1792600000\n";
        assert_eq!(cpu_counters(stat), Some((200, 1000)));
        assert_eq!(last_boot(stat).as_deref(), Some("2026-10-21T16:26:40Z"));
        let meminfo = "MemTotal:  1000 kB\nMemFree: 100 kB\nMemAvailable:  250 kB\n";
        assert_eq!(memory_usage(meminfo), Some(75.0));
    }
}
