//! Picks the server address for the current network: the internal URL on a home
//! network, otherwise the external URL, then the Nabu Casa remote UI URL.

use gio::prelude::*;
use gtk::glib;

use super::server::ServerUrl;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Endpoints {
    pub internal: Option<ServerUrl>,
    pub external: Option<ServerUrl>,
    pub remote_ui: Option<ServerUrl>,
    /// Wi-Fi SSIDs, NetworkManager connection names or interface names.
    pub home_networks: Vec<String>,
}

impl Endpoints {
    /// `primary` is the address used at sign-in and fills any gap.
    /// `networks` holds every name the active connections go by.
    pub fn choose(&self, primary: &ServerUrl, networks: &[String]) -> ServerUrl {
        let at_home = networks
            .iter()
            .any(|network| self.home_networks.contains(network));
        let chosen = if at_home {
            self.internal.as_ref()
        } else {
            self.external.as_ref().or(self.remote_ui.as_ref())
        };
        chosen.unwrap_or(primary).clone()
    }
}

const NM_BUS: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";

/// The active connections' names. Empty when NetworkManager is unavailable.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Networks {
    /// Wi-Fi network names.
    pub ssids: Vec<String>,
    /// NetworkManager connection names, loopback left out.
    pub connections: Vec<String>,
    /// SSIDs, connection names such as "Home VPN", and interface names such
    /// as wg0, for matching against home networks.
    pub names: Vec<String>,
}

pub async fn current_networks() -> Networks {
    match query_networks().await {
        Ok(networks) => networks,
        Err(err) => {
            log::debug!("Network lookup unavailable: {err}");
            Networks::default()
        }
    }
}

impl Networks {
    /// What "Add Current Network" adds: the Wi-Fi network, or the connection
    /// names when not on Wi-Fi.
    pub fn current(&self) -> &[String] {
        if self.ssids.is_empty() {
            &self.connections
        } else {
            &self.ssids
        }
    }
}

async fn query_networks() -> Result<Networks, glib::Error> {
    let bus = gio::bus_get_future(gio::BusType::System).await?;
    let active = property(&bus, NM_PATH, NM_BUS, "ActiveConnections").await?;
    let paths = active
        .get::<Vec<glib::variant::ObjectPath>>()
        .unwrap_or_default();

    let mut networks = Networks::default();
    for path in paths {
        let iface = "org.freedesktop.NetworkManager.Connection.Active";
        if let Some(id) = property(&bus, path.as_str(), iface, "Id")
            .await
            .ok()
            .and_then(|id| id.get::<String>())
        {
            if id != "lo" {
                networks.connections.push(id.clone());
            }
            networks.names.push(id);
        }
        let devices = property(&bus, path.as_str(), iface, "Devices")
            .await
            .ok()
            .and_then(|devices| devices.get::<Vec<glib::variant::ObjectPath>>());
        for device in devices.unwrap_or_default() {
            if let Ok(name) = property(
                &bus,
                device.as_str(),
                "org.freedesktop.NetworkManager.Device",
                "Interface",
            )
            .await
            {
                networks.names.extend(name.get::<String>());
            }
        }
        let kind = property(&bus, path.as_str(), iface, "Type").await?;
        if kind.get::<String>().as_deref() != Some("802-11-wireless") {
            continue;
        }
        // For Wi-Fi connections the specific object is the access point.
        let access_point = property(&bus, path.as_str(), iface, "SpecificObject").await?;
        let Some(access_point) = access_point.get::<glib::variant::ObjectPath>() else {
            continue;
        };
        let ssid = property(
            &bus,
            access_point.as_str(),
            "org.freedesktop.NetworkManager.AccessPoint",
            "Ssid",
        )
        .await?;
        if let Some(bytes) = ssid.get::<Vec<u8>>().filter(|bytes| !bytes.is_empty()) {
            let ssid = String::from_utf8_lossy(&bytes).into_owned();
            networks.names.push(ssid.clone());
            networks.ssids.push(ssid);
        }
    }
    Ok(networks)
}

async fn property(
    bus: &gio::DBusConnection,
    path: &str,
    interface: &str,
    name: &str,
) -> Result<glib::Variant, glib::Error> {
    let reply = bus
        .call_future(
            Some(NM_BUS),
            path,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(interface, name).to_variant()),
            Some(glib::VariantTy::new("(v)").expect("valid type string")),
            gio::DBusCallFlags::NONE,
            2000,
        )
        .await?;
    Ok(reply.child_value(0).as_variant().unwrap_or(reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> ServerUrl {
        ServerUrl::parse(value).unwrap()
    }

    fn endpoints() -> Endpoints {
        Endpoints {
            internal: Some(url("http://ha.local:8123")),
            external: Some(url("https://ha.example.com")),
            remote_ui: Some(url("https://abc.ui.nabu.casa")),
            home_networks: vec!["Home".into()],
        }
    }

    #[test]
    fn home_network_uses_internal() {
        let primary = url("http://10.0.0.2:8123");
        let ssids = vec!["Guest".into(), "Home".into()];
        assert_eq!(
            endpoints().choose(&primary, &ssids),
            url("http://ha.local:8123")
        );
    }

    #[test]
    fn away_uses_external_then_remote_ui() {
        let primary = url("http://10.0.0.2:8123");
        let mut endpoints = endpoints();
        assert_eq!(
            endpoints.choose(&primary, &["Cafe".into()]),
            url("https://ha.example.com")
        );
        endpoints.external = None;
        assert_eq!(
            endpoints.choose(&primary, &[]),
            url("https://abc.ui.nabu.casa")
        );
    }

    #[test]
    fn primary_fills_gaps() {
        let primary = url("http://10.0.0.2:8123");
        assert_eq!(
            Endpoints::default().choose(&primary, &["Home".into()]),
            primary
        );
        let home_only = Endpoints {
            home_networks: vec!["Home".into()],
            ..Endpoints::default()
        };
        assert_eq!(home_only.choose(&primary, &["Home".into()]), primary);
    }
}
