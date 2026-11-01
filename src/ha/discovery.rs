//! Finds Home Assistant servers on the local network through Avahi's D-Bus
//! API. Home Assistant announces itself as `_home-assistant._tcp` with its
//! name and, when set, its internal and external URLs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use serde::Deserialize;

use super::ServerUrl;

const AVAHI: &str = "org.freedesktop.Avahi";
const SERVER: &str = "org.freedesktop.Avahi.Server";
const BROWSER: &str = "org.freedesktop.Avahi.ServiceBrowser";
const SERVICE: &str = "_home-assistant._tcp";
/// Avahi's "any interface" and "any protocol".
const ANY: i32 = -1;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub name: String,
    /// The announced internal URL, or the address and port it was found at.
    pub internal_url: String,
    pub external_url: Option<String>,
    pub version: Option<String>,
}

fn text_records(records: &[Vec<u8>]) -> HashMap<String, String> {
    records
        .iter()
        .filter_map(|record| {
            let text = String::from_utf8_lossy(record);
            let (key, value) = text.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn found(name: &str, host: &str, port: u16, records: &[Vec<u8>]) -> Found {
    let txt = text_records(records);
    let nonempty = |key: &str| txt.get(key).filter(|value| !value.is_empty()).cloned();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Found {
        name: nonempty("location_name").unwrap_or_else(|| name.to_owned()),
        internal_url: nonempty("internal_url").unwrap_or_else(|| format!("http://{host}:{port}")),
        external_url: nonempty("external_url"),
        version: nonempty("version"),
    }
}

/// Browses until dropped. `on_found` runs once per server, matched by name.
pub struct Browser {
    connection: gio::DBusConnection,
    subscription: Option<gio::SignalSubscriptionId>,
    path: Rc<RefCell<Option<String>>>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(id) = self.subscription.take() {
            #[allow(deprecated)]
            self.connection.signal_unsubscribe(id);
        }
        if let Some(path) = self.path.borrow().clone() {
            self.connection.call(
                Some(AVAHI),
                &path,
                BROWSER,
                "Free",
                None,
                None,
                gio::DBusCallFlags::NONE,
                1000,
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }
}

/// `None` when Avahi is not running or not reachable from the sandbox.
pub async fn browse(on_found: impl Fn(Found) + 'static) -> Option<Browser> {
    let connection = gio::bus_get_future(gio::BusType::System).await.ok()?;
    let path: Rc<RefCell<Option<String>>> = Rc::default();
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let on_found = Rc::new(on_found);

    // Subscribed before the browser exists, since Avahi may report items as
    // soon as it is created.
    #[allow(deprecated)]
    let subscription = connection.signal_subscribe(
        Some(AVAHI),
        Some(BROWSER),
        Some("ItemNew"),
        None,
        None,
        gio::DBusSignalFlags::NONE,
        glib::clone!(
            #[strong]
            path,
            move |connection, _, object_path, _, _, parameters| {
                if path
                    .borrow()
                    .as_deref()
                    .is_some_and(|ours| ours != object_path)
                {
                    return;
                }
                let Some((interface, protocol, name, kind, domain, _flags)) =
                    parameters.get::<(i32, i32, String, String, String, u32)>()
                else {
                    return;
                };
                if kind != SERVICE || seen.borrow().contains(&name) {
                    return;
                }
                seen.borrow_mut().push(name.clone());
                let connection = connection.clone();
                let on_found = on_found.clone();
                glib::spawn_future_local(async move {
                    match resolve(&connection, interface, protocol, &name, &kind, &domain).await {
                        Some(server) => on_found(server),
                        None => log::debug!("Could not resolve {name}"),
                    }
                });
            }
        ),
    );

    let reply = connection
        .call_future(
            Some(AVAHI),
            "/",
            SERVER,
            "ServiceBrowserNew",
            Some(&(ANY, ANY, SERVICE, "", 0u32).to_variant()),
            Some(glib::VariantTy::new("(o)").expect("valid type")),
            gio::DBusCallFlags::NONE,
            3000,
        )
        .await;
    let browser_path = match reply {
        Ok(reply) => reply.child_value(0).str().map(str::to_owned),
        Err(err) => {
            log::debug!("Server discovery unavailable: {err}");
            #[allow(deprecated)]
            connection.signal_unsubscribe(subscription);
            return None;
        }
    };
    path.replace(browser_path);
    Some(Browser {
        connection,
        subscription: Some(subscription),
        path,
    })
}

async fn resolve(
    connection: &gio::DBusConnection,
    interface: i32,
    protocol: i32,
    name: &str,
    kind: &str,
    domain: &str,
) -> Option<Found> {
    let reply = connection
        .call_future(
            Some(AVAHI),
            "/",
            SERVER,
            "ResolveService",
            Some(&(interface, protocol, name, kind, domain, ANY, 0u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )
        .await
        .ok()?;
    let host = reply.child_value(7).get::<String>()?;
    let port = reply.child_value(8).get::<u16>()?;
    let records = reply.child_value(9).get::<Vec<Vec<u8>>>()?;
    Some(found(name, &host, port, &records))
}

/// The addresses suggested after sign-in, for the user to confirm.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Addresses {
    pub internal: String,
    pub external: String,
    /// True when the server was reached on the local network.
    pub at_home: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    internal_url: Option<String>,
    external_url: Option<String>,
}

fn host_of(url: &str) -> Option<String> {
    ServerUrl::parse(url).ok().map(|url| url.host().to_owned())
}

/// The announcement for `server`, matched by host.
pub fn matching<'a>(server: &ServerUrl, found: &'a [Found]) -> Option<&'a Found> {
    let host = Some(server.host().to_owned());
    found.iter().find(|candidate| {
        host_of(&candidate.internal_url) == host
            || candidate.external_url.as_deref().and_then(host_of) == host
    })
}

/// The server's configured URLs, then its announcement, then the sign-in
/// address on whichever side it belongs to.
fn suggest(server: &ServerUrl, config: Config, found: Option<&Found>) -> Addresses {
    let nonempty = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    let mut internal = nonempty(config.internal_url)
        .or_else(|| found.map(|found| found.internal_url.clone()))
        .unwrap_or_default();
    let mut external = nonempty(config.external_url)
        .or_else(|| found.and_then(|found| found.external_url.clone()))
        .unwrap_or_default();
    let private = server.is_private();
    if private && internal.is_empty() {
        internal = server.as_str().to_owned();
    } else if !private && external.is_empty() {
        external = server.as_str().to_owned();
    }
    Addresses {
        internal,
        external,
        at_home: private || found.is_some(),
    }
}

/// Asks the server for its configured URLs. Failures leave them out.
pub async fn addresses(server: &ServerUrl, token: &str, found: &[Found]) -> Addresses {
    let config = match super::http::get(&server.join("/api/config"), Some(token)).await {
        Ok(response) if response.is_success() => response.json().unwrap_or_default(),
        Ok(response) => {
            log::debug!("Server configuration unavailable: HTTP {}", response.status);
            Config::default()
        }
        Err(err) => {
            log::debug!("Server configuration unavailable: {err}");
            Config::default()
        }
    };
    suggest(server, config, matching(server, found))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_announcement() {
        let records: Vec<Vec<u8>> = [
            "location_name=Home",
            "internal_url=http://192.168.1.5:8123",
            "external_url=",
            "version=2026.2.3",
        ]
        .iter()
        .map(|record| record.as_bytes().to_vec())
        .collect();
        let server = found("Home._home-assistant._tcp", "192.168.1.5", 8123, &records);
        assert_eq!(server.name, "Home");
        assert_eq!(server.internal_url, "http://192.168.1.5:8123");
        assert_eq!(server.external_url, None);

        let bare = found("ha", "fe80::1", 8123, &[]);
        assert_eq!(bare.internal_url, "http://[fe80::1]:8123");
    }

    #[test]
    fn suggests_addresses() {
        let local = ServerUrl::parse("http://192.168.1.5:8123").unwrap();
        let remote = ServerUrl::parse("https://ha.example.com").unwrap();
        let announced = found("Home", "192.168.1.5", 8123, &[]);

        let bare = suggest(&local, Config::default(), None);
        assert_eq!(bare.internal, "http://192.168.1.5:8123");
        assert_eq!(bare.external, "");
        assert!(bare.at_home);

        let configured = Config {
            internal_url: Some("http://homeassistant.local:8123".into()),
            external_url: Some("https://ha.example.com".into()),
        };
        let both = suggest(&remote, configured, None);
        assert_eq!(both.internal, "http://homeassistant.local:8123");
        assert_eq!(both.external, "https://ha.example.com");
        assert!(!both.at_home);

        let list = [announced];
        assert!(matching(&local, &list).is_some());
        assert!(matching(&remote, &list).is_none());
        let away = suggest(&remote, Config::default(), None);
        assert_eq!(away.external, "https://ha.example.com");
        assert_eq!(away.internal, "");
    }
}
