//! Background portal: permission to keep running without a window, and the
//! autostart entry used by "Start at Login".

use std::collections::HashMap;

use futures_util::StreamExt;
use gtk::gio;
use gtk::glib;
use gtk::glib::variant::ToVariant;

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Granted {
    pub background: bool,
    pub autostart: bool,
}

/// Autostart launches `sanctum --background`.
pub async fn request(autostart: bool, reason: &str) -> Result<Granted, glib::Error> {
    let connection = gio::bus_get_future(gio::BusType::Session).await?;
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

    let options = glib::VariantDict::new(None);
    options.insert("handle_token", &token);
    options.insert("reason", reason);
    options.insert("autostart", autostart);
    options.insert("commandline", vec!["sanctum", "--background"]);
    options.insert("dbus-activatable", false);

    connection
        .call_future(
            Some(PORTAL_BUS),
            PORTAL_PATH,
            "org.freedesktop.portal.Background",
            "RequestBackground",
            Some(&glib::Variant::tuple_from_iter([
                "".to_variant(),
                options.end(),
            ])),
            None,
            gio::DBusCallFlags::NONE,
            -1,
        )
        .await?;

    let Some(Ok((code, results))) = responses.next().await else {
        return Err(glib::Error::new(
            gio::IOErrorEnum::Failed,
            "no portal response",
        ));
    };
    if code != 0 {
        return Ok(Granted {
            background: false,
            autostart: false,
        });
    }
    let flag = |key: &str| {
        results
            .get(key)
            .and_then(|v| v.get::<bool>())
            .unwrap_or(false)
    };
    Ok(Granted {
        background: flag("background"),
        autostart: flag("autostart"),
    })
}
