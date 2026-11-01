use std::collections::HashMap;

use gtk::glib;
use gtk::glib::prelude::*;

use super::Switch;
use crate::agent::dbus;

pub const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";

type Objects = HashMap<glib::variant::ObjectPath, HashMap<String, HashMap<String, glib::Variant>>>;

const DEVICE: &str = "org.bluez.Device1";

async fn objects() -> Option<Objects> {
    let system = dbus::system().await?;
    let reply = dbus::call(
        &system,
        BLUEZ,
        "/",
        "org.freedesktop.DBus.ObjectManager",
        "GetManagedObjects",
        None,
    )
    .await
    .ok()?;
    reply.child_value(0).get()
}

pub async fn adapter() -> Option<(String, bool)> {
    let objects = objects().await?;
    let mut adapters: Vec<(String, bool)> = objects
        .into_iter()
        .filter_map(|(path, interfaces)| {
            let powered = interfaces.get(ADAPTER)?.get("Powered")?.get::<bool>()?;
            Some((path.as_str().to_owned(), powered))
        })
        .collect();
    adapters.sort();
    adapters.into_iter().next()
}

pub struct State {
    pub powered: bool,
    /// Names of connected devices.
    pub connected: Vec<String>,
}

pub async fn state() -> Option<State> {
    let objects = objects().await?;
    let powered = objects
        .values()
        .filter_map(|interfaces| interfaces.get(ADAPTER)?.get("Powered")?.get::<bool>())
        .next()?;
    let mut connected: Vec<String> = objects
        .values()
        .filter_map(|interfaces| {
            let device = interfaces.get(DEVICE)?;
            if !device.get("Connected")?.get::<bool>()? {
                return None;
            }
            ["Alias", "Name", "Address"]
                .iter()
                .find_map(|key| device.get(*key)?.get::<String>())
        })
        .collect();
    connected.sort();
    Some(State { powered, connected })
}

pub async fn set(switch: Switch) -> Result<(), String> {
    let (path, powered) = adapter().await.ok_or("no Bluetooth adapter was found")?;
    let system = dbus::system()
        .await
        .ok_or("the system bus is not reachable")?;
    dbus::call(
        &system,
        BLUEZ,
        &path,
        "org.freedesktop.DBus.Properties",
        "Set",
        Some((ADAPTER, "Powered", switch.resolve(powered).to_variant()).to_variant()),
    )
    .await
    .map(drop)
    .map_err(|err| err.message().to_owned())
}
