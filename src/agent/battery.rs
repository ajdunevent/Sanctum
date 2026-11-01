//! Battery level and state from UPower, with sysfs as a fallback, and the
//! charger type from sysfs, which UPower does not report.

use std::fs;
use std::path::Path;

use gtk::gio;

use super::dbus::{self, PropertyMap};
use super::sensor::{Reading, battery_icon};

pub const UPOWER: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const POWER_SUPPLY: &str = "/sys/class/power_supply";
/// UPower device type for batteries.
const TYPE_BATTERY: u32 = 2;

struct Battery {
    level: f64,
    /// `charging`, `discharging`, `full` or `not_charging`, as on Android.
    state: &'static str,
}

pub async fn read(system: Option<&gio::DBusConnection>) -> Vec<Reading> {
    let battery = match system {
        Some(system) => from_upower(system).await,
        None => None,
    }
    .or_else(from_sysfs);
    let Some(battery) = battery else {
        return Vec::new();
    };
    let charging = battery.state == "charging";
    vec![
        Reading::new("battery_level", battery.level.round() as i64)
            .icon(battery_icon(battery.level, charging)),
        Reading::new("battery_state", battery.state),
        Reading::new("charger_type", charger_type()),
    ]
}

async fn from_upower(system: &gio::DBusConnection) -> Option<Battery> {
    let props = dbus::properties(
        system,
        UPOWER,
        DISPLAY_DEVICE,
        "org.freedesktop.UPower.Device",
    )
    .await?;
    if !props.value::<bool>("IsPresent")? || props.value::<u32>("Type")? != TYPE_BATTERY {
        return None;
    }
    // UPower states: 1 charging, 2 discharging, 3 empty, 4 fully charged,
    // 5 pending charge, 6 pending discharge.
    let state = match props.value::<u32>("State")? {
        1 => "charging",
        2 | 3 | 6 => "discharging",
        4 => "full",
        _ => "not_charging",
    };
    Some(Battery {
        level: props.value::<f64>("Percentage")?,
        state,
    })
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn supplies() -> Vec<std::path::PathBuf> {
    fs::read_dir(POWER_SUPPLY)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

fn from_sysfs() -> Option<Battery> {
    let supply = supplies().into_iter().find(|path| {
        read_trimmed(&path.join("type")).as_deref() == Some("Battery")
            && read_trimmed(&path.join("present")).as_deref() != Some("0")
    })?;
    let level = read_trimmed(&supply.join("capacity"))?.parse().ok()?;
    let state = match read_trimmed(&supply.join("status")).as_deref() {
        Some("Charging") => "charging",
        Some("Discharging") => "discharging",
        Some("Full") => "full",
        _ => "not_charging",
    };
    Some(Battery { level, state })
}

/// `ac`, `usb`, `wireless` or `none`, from the first online supply.
fn charger_type() -> &'static str {
    for supply in supplies() {
        if read_trimmed(&supply.join("online")).as_deref() != Some("1") {
            continue;
        }
        match read_trimmed(&supply.join("type")).as_deref() {
            Some("Mains") => return "ac",
            Some(kind) if kind.starts_with("USB") => return "usb",
            Some("Wireless") => return "wireless",
            _ => {}
        }
    }
    "none"
}
