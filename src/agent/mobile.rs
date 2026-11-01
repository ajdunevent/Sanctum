//! Signal, operator and access technology of the first modem in ModemManager.

use std::collections::HashMap;

use gtk::gio;
use gtk::glib;

use super::dbus::{self, PropertyMap};
use super::sensor::Reading;

pub const MODEM_MANAGER: &str = "org.freedesktop.ModemManager1";
const ROOT: &str = "/org/freedesktop/ModemManager1";
const MODEM: &str = "org.freedesktop.ModemManager1.Modem";
const MODEM_3GPP: &str = "org.freedesktop.ModemManager1.Modem.Modem3gpp";

type Interfaces = HashMap<String, HashMap<String, glib::Variant>>;

pub async fn read(system: &gio::DBusConnection) -> Vec<Reading> {
    let Ok(reply) = dbus::call(
        system,
        MODEM_MANAGER,
        ROOT,
        "org.freedesktop.DBus.ObjectManager",
        "GetManagedObjects",
        None,
    )
    .await
    else {
        return Vec::new();
    };
    let objects = reply.child_value(0);
    let mut modems: Vec<(String, Interfaces)> = objects
        .iter()
        .filter_map(|entry| {
            let path = entry.child_value(0).str()?.to_owned();
            let interfaces = entry.child_value(1).get::<Interfaces>()?;
            interfaces.contains_key(MODEM).then_some((path, interfaces))
        })
        .collect();
    modems.sort_by(|a, b| a.0.cmp(&b.0));
    let Some((_, interfaces)) = modems.into_iter().next() else {
        return Vec::new();
    };

    let modem = &interfaces[MODEM];
    let signal = modem
        .get("SignalQuality")
        .and_then(|value| value.get::<(u32, bool)>())
        .map_or(0, |(quality, _)| quality);
    let technology = access_technology(modem.value::<u32>("AccessTechnologies").unwrap_or(0));
    let operator = interfaces
        .get(MODEM_3GPP)
        .and_then(|props| props.value::<String>("OperatorName"))
        .unwrap_or_default();
    vec![
        Reading::new("mobile_signal_strength", signal),
        Reading::new("mobile_operator", operator),
        Reading::new("mobile_network_type", technology),
    ]
}

/// The newest technology in ModemManager's access technology bit mask.
fn access_technology(mask: u32) -> &'static str {
    const NAMES: [(u32, &str); 12] = [
        (1 << 15, "5g"),
        (1 << 14, "lte"),
        (1 << 16, "lte_cat_m"),
        (1 << 17, "lte_nb_iot"),
        (1 << 9, "hspa_plus"),
        (1 << 8, "hspa"),
        (1 << 7, "hsupa"),
        (1 << 6, "hsdpa"),
        (1 << 5, "umts"),
        (1 << 4, "edge"),
        (1 << 3, "gprs"),
        (1 << 1, "gsm"),
    ];
    NAMES
        .iter()
        .find(|(bit, _)| mask & bit != 0)
        .map_or("none", |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_technology_wins() {
        assert_eq!(access_technology(0), "none");
        assert_eq!(access_technology((1 << 14) | (1 << 5)), "lte");
        assert_eq!(access_technology((1 << 15) | (1 << 14)), "5g");
        assert_eq!(access_technology(1 << 4), "edge");
    }
}
