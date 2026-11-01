//! Small helpers over GDBus for reading service properties and watching them
//! change.

use std::collections::HashMap;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

const TIMEOUT_MS: i32 = 5_000;

pub async fn system() -> Option<gio::DBusConnection> {
    gio::bus_get_future(gio::BusType::System).await.ok()
}

pub async fn session() -> Option<gio::DBusConnection> {
    gio::bus_get_future(gio::BusType::Session).await.ok()
}

pub async fn call(
    connection: &gio::DBusConnection,
    name: &str,
    path: &str,
    interface: &str,
    method: &str,
    parameters: Option<glib::Variant>,
) -> Result<glib::Variant, glib::Error> {
    connection
        .call_future(
            Some(name),
            path,
            interface,
            method,
            parameters.as_ref(),
            None,
            gio::DBusCallFlags::NONE,
            TIMEOUT_MS,
        )
        .await
}

/// All properties of one interface, or `None` when the service or object is
/// missing.
pub async fn properties(
    connection: &gio::DBusConnection,
    name: &str,
    path: &str,
    interface: &str,
) -> Option<HashMap<String, glib::Variant>> {
    let reply = call(
        connection,
        name,
        path,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        Some((interface,).to_variant()),
    )
    .await
    .ok()?;
    reply.child_value(0).get::<HashMap<String, glib::Variant>>()
}

pub async fn has_owner(connection: &gio::DBusConnection, name: &str) -> bool {
    call(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some((name,).to_variant()),
    )
    .await
    .ok()
    .and_then(|reply| reply.child_value(0).get::<bool>())
    .unwrap_or(false)
}

async fn bus_names(connection: &gio::DBusConnection, method: &str) -> Vec<String> {
    call(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        method,
        None,
    )
    .await
    .ok()
    .and_then(|reply| reply.child_value(0).get::<Vec<String>>())
    .unwrap_or_default()
}

/// Inside Flatpak only permitted names are listed.
pub async fn names(connection: &gio::DBusConnection) -> Vec<String> {
    bus_names(connection, "ListNames").await
}

/// dconf and similar services are D-Bus activated and often have no owner yet.
pub async fn service_exists(connection: &gio::DBusConnection, name: &str) -> bool {
    has_owner(connection, name).await
        || bus_names(connection, "ListActivatableNames")
            .await
            .iter()
            .any(|activatable| activatable == name)
}

/// Calls `f` for signals from `sender`, narrowed by interface, member and
/// first string argument when given.
pub fn watch(
    connection: &gio::DBusConnection,
    sender: &str,
    interface: Option<&str>,
    member: Option<&str>,
    arg0: Option<&str>,
    f: impl Fn() + 'static,
) -> gio::SignalSubscription {
    connection.subscribe_to_signal(
        Some(sender),
        interface,
        member,
        None,
        arg0,
        gio::DBusSignalFlags::NONE,
        move |_| f(),
    )
}

pub trait PropertyMap {
    fn value<T: glib::variant::FromVariant>(&self, key: &str) -> Option<T>;
}

impl PropertyMap for HashMap<String, glib::Variant> {
    fn value<T: glib::variant::FromVariant>(&self, key: &str) -> Option<T> {
        self.get(key).and_then(|value| value.get::<T>())
    }
}

/// Byte-array strings such as SSIDs; invalid UTF-8 is replaced.
pub fn bytes_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .to_owned()
}
