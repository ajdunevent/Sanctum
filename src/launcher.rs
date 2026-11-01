//! An optional "Assist" launcher in the app grid, installed through the
//! Dynamic Launcher portal so the user can add or remove it from Preferences.

use std::collections::HashMap;

use futures_util::StreamExt;
use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use gtk::glib::variant::ToVariant;

use crate::agent::dbus;
use crate::config::APP_ID;

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const LAUNCHER: &str = "org.freedesktop.portal.DynamicLauncher";
const ICON: &[u8] = include_bytes!("../data/icons/assist-launcher.svg");
/// The portal's launcher_type for an application.
const APPLICATION: u32 = 1;

fn desktop_id() -> String {
    format!("{APP_ID}.Assist.desktop")
}

fn entry(name: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nComment=Talk to Home Assistant\n\
         Exec=sanctum --assist\nTerminal=false\nStartupNotify=true\n\
         Categories=Utility;GTK;\nKeywords=Home Assistant;assist;voice;\n"
    )
}

async fn session() -> Result<gio::DBusConnection, glib::Error> {
    gio::bus_get_future(gio::BusType::Session).await
}

/// Whether the desktop's portal offers dynamic launchers.
pub async fn is_available() -> bool {
    let Ok(connection) = session().await else {
        return false;
    };
    dbus::call(
        &connection,
        PORTAL_BUS,
        PORTAL_PATH,
        "org.freedesktop.DBus.Properties",
        "Get",
        Some((LAUNCHER, "version").to_variant()),
    )
    .await
    .is_ok()
}

pub async fn is_installed() -> bool {
    let Ok(connection) = session().await else {
        return false;
    };
    dbus::call(
        &connection,
        PORTAL_BUS,
        PORTAL_PATH,
        LAUNCHER,
        "GetDesktopEntry",
        Some((desktop_id(),).to_variant()),
    )
    .await
    .is_ok()
}

/// Asks the user to confirm, then installs the launcher. `Ok(false)` means
/// the user cancelled.
pub async fn install(name: &str) -> Result<bool, glib::Error> {
    let connection = session().await?;
    let sender = connection
        .unique_name()
        .map(|name| name.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let token = format!("sanctum{}", glib::random_int_range(0, i32::MAX));
    let handle = format!("{PORTAL_PATH}/request/{sender}/{token}");
    // Subscribed before the call so a fast response is not missed.
    let mut responses = connection
        .receive_signal_parameters::<(u32, HashMap<String, glib::Variant>)>(
            Some(PORTAL_BUS),
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(&handle),
            None,
            gio::DBusSignalFlags::NONE,
        );

    let icon = gio::BytesIcon::new(&glib::Bytes::from_static(ICON))
        .serialize()
        .ok_or_else(|| glib::Error::new(gio::IOErrorEnum::Failed, "icon"))?;
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", &token);
    options.insert("modal", true);
    options.insert("launcher_type", APPLICATION);
    options.insert("editable_name", true);
    options.insert("editable_icon", false);
    dbus::call(
        &connection,
        PORTAL_BUS,
        PORTAL_PATH,
        LAUNCHER,
        "PrepareInstall",
        Some(glib::Variant::tuple_from_iter([
            "".to_variant(),
            name.to_variant(),
            glib::Variant::from_variant(&icon),
            options.end(),
        ])),
    )
    .await?;

    let Some(Ok((code, results))) = responses.next().await else {
        return Err(glib::Error::new(
            gio::IOErrorEnum::Failed,
            "no portal response",
        ));
    };
    if code != 0 {
        return Ok(false);
    }
    let token = results
        .get("token")
        .and_then(|value| value.get::<String>())
        .ok_or_else(|| glib::Error::new(gio::IOErrorEnum::Failed, "no install token"))?;
    let chosen = results
        .get("name")
        .and_then(|value| value.get::<String>())
        .filter(|chosen| !chosen.trim().is_empty())
        .unwrap_or_else(|| name.to_owned());
    dbus::call(
        &connection,
        PORTAL_BUS,
        PORTAL_PATH,
        LAUNCHER,
        "Install",
        Some(glib::Variant::tuple_from_iter([
            token.to_variant(),
            desktop_id().to_variant(),
            entry(&chosen).to_variant(),
            glib::VariantDict::new(None).end(),
        ])),
    )
    .await?;
    Ok(true)
}

pub async fn uninstall() -> Result<(), glib::Error> {
    let connection = session().await?;
    dbus::call(
        &connection,
        PORTAL_BUS,
        PORTAL_PATH,
        LAUNCHER,
        "Uninstall",
        Some(glib::Variant::tuple_from_iter([
            desktop_id().to_variant(),
            glib::VariantDict::new(None).end(),
        ])),
    )
    .await
    .map(drop)
}
