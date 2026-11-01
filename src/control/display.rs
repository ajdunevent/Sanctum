//! Brightness is set through logind's `SetBrightness`, which lets the active
//! session's user write sysfs devices without extra privileges. On FuriOS the
//! torch belongs to flashlightd on the session bus, since its LED is not a
//! standard sysfs device.

use std::fs;
use std::path::{Path, PathBuf};

use gtk::gio;
use gtk::glib::prelude::*;

use super::Switch;
use crate::agent::dbus::{self, PropertyMap};
use crate::agent::system::{
    DISPLAY_CONFIG, DISPLAY_CONFIG_PATH, LOGIND, LOGIND_SESSION, OWN_SESSION,
};

const SYSFS: &str = "/sys/class";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub subsystem: &'static str,
    pub name: String,
    pub brightness: u32,
    pub max: u32,
}

impl Device {
    fn read(root: &Path, subsystem: &'static str, name: &str) -> Option<Self> {
        let dir = root.join(subsystem).join(name);
        let value = |file: &str| -> Option<u32> {
            fs::read_to_string(dir.join(file)).ok()?.trim().parse().ok()
        };
        Some(Self {
            subsystem,
            name: name.to_owned(),
            brightness: value("brightness")?,
            max: value("max_brightness").filter(|max| *max > 0)?,
        })
    }

    pub fn percent(&self) -> u32 {
        (f64::from(self.brightness) / f64::from(self.max) * 100.0).round() as u32
    }
}

fn entries(root: &Path, subsystem: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root.join(subsystem))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Firmware interfaces are preferred over platform and raw ones, as in GNOME.
/// Phones running Android kernels expose the panel as an LED, such as
/// `leds/lcd-backlight`.
fn backlight_in(root: &Path) -> Option<Device> {
    let rank = |name: &String| {
        let kind =
            fs::read_to_string(root.join("backlight").join(name).join("type")).unwrap_or_default();
        match kind.trim() {
            "firmware" => 0,
            "platform" => 1,
            _ => 2,
        }
    };
    let mut names = entries(root, "backlight");
    names.sort_by_key(rank);
    names
        .iter()
        .find_map(|name| Device::read(root, "backlight", name))
        .or_else(|| {
            entries(root, "leds")
                .iter()
                .filter(|name| name.contains("backlight"))
                .find_map(|name| Device::read(root, "leds", name))
        })
}

/// Torch LEDs are preferred because flash LEDs may only be rated for short
/// pulses.
fn flashlight_in(root: &Path) -> Option<Device> {
    let names = entries(root, "leds");
    let named = |word: &str| {
        names
            .iter()
            .filter(|name| name.to_lowercase().contains(word))
            .find_map(|name| Device::read(root, "leds", name))
    };
    named("torch").or_else(|| named("flash"))
}

pub fn backlight() -> Option<Device> {
    backlight_in(&PathBuf::from(SYSFS))
}

pub const FLASHLIGHTD: &str = "io.furios.Flashlightd";
const FLASHLIGHTD_PATH: &str = "/io/furios/Flashlightd";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Torch {
    Flashlightd { brightness: u32, max: u32 },
    Sysfs(Device),
}

impl Torch {
    pub fn is_on(&self) -> bool {
        match self {
            Self::Flashlightd { brightness, .. } => *brightness > 0,
            Self::Sysfs(device) => device.brightness > 0,
        }
    }
}

async fn flashlightd() -> Option<Torch> {
    let session = dbus::session().await?;
    if !dbus::has_owner(&session, FLASHLIGHTD).await {
        return None;
    }
    let properties = dbus::properties(&session, FLASHLIGHTD, FLASHLIGHTD_PATH, FLASHLIGHTD).await?;
    Some(Torch::Flashlightd {
        brightness: properties.value("Brightness")?,
        max: properties.value::<u32>("MaxBrightness")?.max(1),
    })
}

pub async fn flashlight() -> Option<Torch> {
    match flashlightd().await {
        Some(torch) => Some(torch),
        None => flashlight_in(&PathBuf::from(SYSFS)).map(Torch::Sysfs),
    }
}

async fn system() -> Result<gio::DBusConnection, String> {
    dbus::system()
        .await
        .ok_or_else(|| "the system bus is not reachable".to_owned())
}

async fn set(device: &Device, value: u32) -> Result<(), String> {
    dbus::call(
        &system().await?,
        LOGIND,
        OWN_SESSION,
        LOGIND_SESSION,
        "SetBrightness",
        Some((device.subsystem, device.name.as_str(), value).to_variant()),
    )
    .await
    .map(drop)
    .map_err(|err| err.message().to_owned())
}

pub async fn set_brightness(percent: u32) -> Result<(), String> {
    let device = backlight().ok_or("no adjustable backlight was found")?;
    // A value of zero leaves some panels fully dark.
    let value = (f64::from(device.max) * f64::from(percent) / 100.0).round() as u32;
    set(&device, value.max(1)).await
}

pub async fn set_flashlight(switch: Switch) -> Result<(), String> {
    let torch = flashlight().await.ok_or("no flashlight LED was found")?;
    let on = switch.resolve(torch.is_on());
    match torch {
        Torch::Sysfs(device) => set(&device, if on { device.max } else { 0 }).await,
        Torch::Flashlightd { max, .. } => {
            let session = dbus::session()
                .await
                .ok_or("the session bus is not reachable")?;
            dbus::call(
                &session,
                FLASHLIGHTD,
                FLASHLIGHTD_PATH,
                FLASHLIGHTD,
                "SetBrightness",
                Some((if on { max } else { 0 },).to_variant()),
            )
            .await
            .map(drop)
            .map_err(|err| err.message().to_owned())
        }
    }
}

pub async fn lock() -> Result<(), String> {
    dbus::call(
        &system().await?,
        LOGIND,
        OWN_SESSION,
        LOGIND_SESSION,
        "Lock",
        None,
    )
    .await
    .map(drop)
    .map_err(|err| err.message().to_owned())
}

/// Writes to the power save mode are accepted by both Phosh and Mutter.
pub async fn set_screen_on(on: bool) -> Result<(), String> {
    let session = dbus::session()
        .await
        .ok_or("the session bus is not reachable")?;
    let mode: i32 = if on { 0 } else { 3 };
    dbus::call(
        &session,
        DISPLAY_CONFIG,
        DISPLAY_CONFIG_PATH,
        "org.freedesktop.DBus.Properties",
        "Set",
        Some((DISPLAY_CONFIG, "PowerSaveMode", mode.to_variant()).to_variant()),
    )
    .await
    .map(drop)
    .map_err(|err| err.message().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(root: &Path, path: &str, brightness: u32, max: u32, kind: Option<&str>) {
        let dir = root.join(path);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("brightness"), format!("{brightness}\n")).unwrap();
        fs::write(dir.join("max_brightness"), format!("{max}\n")).unwrap();
        if let Some(kind) = kind {
            fs::write(dir.join("type"), kind).unwrap();
        }
    }

    #[test]
    fn finds_devices() {
        let root = std::env::temp_dir().join(format!("sanctum-sysfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(backlight_in(&root), None);
        device(&root, "backlight/raw0", 10, 100, Some("raw"));
        device(&root, "backlight/panel", 128, 255, Some("platform"));
        device(&root, "leds/white:flash", 0, 200, None);
        device(&root, "leds/white:torch", 0, 15, None);
        device(&root, "leds/red:status", 1, 1, None);
        device(&root, "leds/lcd-backlight", 1024, 2047, None);
        let backlight = backlight_in(&root).unwrap();
        assert_eq!(backlight.name, "panel");
        assert_eq!(backlight.percent(), 50);
        let flash = flashlight_in(&root).unwrap();
        assert_eq!(
            (flash.subsystem, flash.name.as_str(), flash.max),
            ("leds", "white:torch", 15)
        );
        fs::remove_dir_all(root.join("backlight")).unwrap();
        let panel = backlight_in(&root).unwrap();
        assert_eq!(
            (panel.subsystem, panel.name.as_str()),
            ("leds", "lcd-backlight")
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
