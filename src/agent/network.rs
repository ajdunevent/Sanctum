//! Connection type, Wi-Fi details and addresses from NetworkManager.

use std::collections::HashMap;

use gtk::gio;
use gtk::glib;

use super::dbus::{self, PropertyMap};
use super::sensor::Reading;

pub const NETWORK_MANAGER: &str = "org.freedesktop.NetworkManager";
const MANAGER_PATH: &str = "/org/freedesktop/NetworkManager";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const WIRELESS: &str = "802-11-wireless";
/// Shown for the SSID when Wi-Fi is not connected, as the Android app does.
const NOT_CONNECTED: &str = "<not connected>";

type Props = HashMap<String, glib::Variant>;

pub async fn read(system: &gio::DBusConnection) -> Vec<Reading> {
    let Some(manager) =
        dbus::properties(system, NETWORK_MANAGER, MANAGER_PATH, NETWORK_MANAGER).await
    else {
        return Vec::new();
    };

    let primary_type = manager
        .value::<String>("PrimaryConnectionType")
        .unwrap_or_default();
    let mut readings = vec![Reading::new("network_type", network_type(&primary_type))];

    let mut active = Vec::new();
    for path in object_paths(&manager, "ActiveConnections") {
        if let Some(props) = dbus::properties(system, NETWORK_MANAGER, &path, ACTIVE).await {
            active.push((path, props));
        }
    }

    let wifi = active
        .iter()
        .find(|(_, props)| props.value::<String>("Type").as_deref() == Some(WIRELESS));
    let access_point = match wifi.and_then(|(_, props)| object_path(props, "SpecificObject")) {
        Some(path) => {
            dbus::properties(
                system,
                NETWORK_MANAGER,
                &path,
                "org.freedesktop.NetworkManager.AccessPoint",
            )
            .await
        }
        None => None,
    };
    match access_point {
        Some(ap) => {
            let ssid = ap
                .value::<Vec<u8>>("Ssid")
                .map(|bytes| dbus::bytes_to_string(&bytes))
                .unwrap_or_default();
            readings.push(Reading::new("wifi_connection", ssid));
            readings.push(Reading::new(
                "wifi_bssid",
                ap.value::<String>("HwAddress")
                    .unwrap_or_default()
                    .to_lowercase(),
            ));
            readings.push(Reading::new(
                "wifi_signal_strength",
                ap.value::<u8>("Strength").unwrap_or(0),
            ));
            readings.push(Reading::new(
                "wifi_frequency",
                ap.value::<u32>("Frequency").unwrap_or(0),
            ));
        }
        None => {
            readings.push(Reading::new("wifi_connection", NOT_CONNECTED));
            readings.push(Reading::new("wifi_bssid", NOT_CONNECTED));
            readings.push(Reading::new("wifi_signal_strength", 0));
            readings.push(Reading::new("wifi_frequency", 0));
        }
    }

    // Addresses come from the primary connection, or the first active one.
    let primary = object_path(&manager, "PrimaryConnection");
    let source = active
        .iter()
        .find(|(path, _)| Some(path) == primary.as_ref())
        .or_else(|| active.first())
        .map(|(_, props)| props);
    let (ipv4, ipv6) = match source {
        Some(props) => (
            address(
                system,
                props,
                "Ip4Config",
                "org.freedesktop.NetworkManager.IP4Config",
            )
            .await,
            address(
                system,
                props,
                "Ip6Config",
                "org.freedesktop.NetworkManager.IP6Config",
            )
            .await,
        ),
        None => (None, None),
    };
    readings.push(Reading::new("ipv4_address", ipv4.unwrap_or_default()));
    readings.push(Reading::new("ipv6_address", ipv6.unwrap_or_default()));
    readings
}

fn network_type(connection_type: &str) -> &str {
    match connection_type {
        "" => "none",
        WIRELESS => "wifi",
        "802-3-ethernet" => "ethernet",
        "gsm" | "cdma" => "cellular",
        "vpn" | "wireguard" => "vpn",
        "bluetooth" => "bluetooth",
        other => other,
    }
}

fn object_path(props: &Props, key: &str) -> Option<String> {
    let path = props.get(key)?.str()?.to_owned();
    (path != "/").then_some(path)
}

fn object_paths(props: &Props, key: &str) -> Vec<String> {
    props
        .get(key)
        .map(|value| {
            value
                .iter()
                .filter_map(|item| item.str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The first global address of an IP config; link-local IPv6 is skipped.
async fn address(
    system: &gio::DBusConnection,
    connection: &Props,
    key: &str,
    interface: &str,
) -> Option<String> {
    let path = object_path(connection, key)?;
    let config = dbus::properties(system, NETWORK_MANAGER, &path, interface).await?;
    let data = config.get("AddressData")?;
    data.iter()
        .filter_map(|entry| entry.get::<HashMap<String, glib::Variant>>())
        .filter_map(|entry| entry.value::<String>("address"))
        .find(|address| !address.starts_with("fe80:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_types() {
        assert_eq!(network_type(""), "none");
        assert_eq!(network_type("802-11-wireless"), "wifi");
        assert_eq!(network_type("gsm"), "cellular");
        assert_eq!(network_type("wireguard"), "vpn");
    }
}
