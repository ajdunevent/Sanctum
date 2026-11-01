//! The sensors Sanctum can report and the readings sources produce for them.

use gettextrs::gettext;
use gtk::glib;
use serde_json::{Map, Value, json};

use crate::control::sandbox::Permission;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sensor,
    BinarySensor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Battery,
    Network,
    Mobile,
    System,
    Controls,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Battery,
        Self::Network,
        Self::Mobile,
        Self::System,
        Self::Controls,
    ];

    pub fn title(self) -> String {
        match self {
            Self::Battery => gettext("Battery and Power"),
            Self::Network => gettext("Network"),
            Self::Mobile => gettext("Mobile Network"),
            Self::System => gettext("System and Session"),
            Self::Controls => gettext("Sound, Screen and Hardware"),
        }
    }
}

#[derive(Debug)]
pub struct Definition {
    /// Stable unique ID sent to Home Assistant; changing it creates a new entity.
    pub id: &'static str,
    pub kind: Kind,
    pub category: Category,
    pub icon: &'static str,
    pub device_class: Option<&'static str>,
    pub unit: Option<&'static str>,
    pub state_class: Option<&'static str>,
    pub entity_category: Option<&'static str>,
}

impl Definition {
    /// The sandbox permission the sensor cannot be read without. Battery
    /// sensors fall back to sysfs, so they need none.
    pub fn permission(&self) -> Option<Permission> {
        Some(match self.id {
            "network_type"
            | "wifi_connection"
            | "wifi_bssid"
            | "wifi_signal_strength"
            | "wifi_frequency"
            | "ipv4_address"
            | "ipv6_address" => Permission::SystemTalk(super::network::NETWORK_MANAGER),
            "mobile_signal_strength" | "mobile_operator" | "mobile_network_type" => {
                Permission::SystemTalk(super::mobile::MODEM_MANAGER)
            }
            "screen_on" => Permission::SessionTalk(super::system::DISPLAY_CONFIG),
            "session_locked" => Permission::SystemTalk(super::system::LOGIND),
            "volume_level" | "volume_muted" => Permission::Socket("pulseaudio"),
            "ringer_mode" => Permission::SessionTalk(crate::control::ringer::FEEDBACKD),
            "bluetooth_state" | "bluetooth_connection" => {
                Permission::SystemTalk(crate::control::bluetooth::BLUEZ)
            }
            "media_session" => Permission::SessionTalk(crate::control::media::MPRIS),
            "do_not_disturb" => Permission::Filesystem(crate::control::settings::DATABASE),
            _ => return None,
        })
    }

    pub fn name(&self) -> String {
        match self.id {
            "battery_level" => gettext("Battery Level"),
            "battery_state" => gettext("Battery State"),
            "charger_type" => gettext("Charger Type"),
            "network_type" => gettext("Network Type"),
            "wifi_connection" => gettext("Wi-Fi Connection"),
            "wifi_bssid" => gettext("Wi-Fi BSSID"),
            "wifi_signal_strength" => gettext("Wi-Fi Signal Strength"),
            "wifi_frequency" => gettext("Wi-Fi Frequency"),
            "ipv4_address" => gettext("IPv4 Address"),
            "ipv6_address" => gettext("IPv6 Address"),
            "mobile_signal_strength" => gettext("Mobile Signal Strength"),
            "mobile_operator" => gettext("Mobile Operator"),
            "mobile_network_type" => gettext("Mobile Network Type"),
            "screen_on" => gettext("Screen On"),
            "session_locked" => gettext("Session Locked"),
            "cpu_usage" => gettext("CPU Usage"),
            "memory_usage" => gettext("Memory Usage"),
            "last_boot" => gettext("Last Boot"),
            "volume_level" => gettext("Volume Level"),
            "volume_muted" => gettext("Volume Muted"),
            "screen_brightness" => gettext("Screen Brightness"),
            "flashlight" => gettext("Flashlight"),
            "ringer_mode" => gettext("Ringer Mode"),
            "bluetooth_state" => gettext("Bluetooth"),
            "bluetooth_connection" => gettext("Bluetooth Connections"),
            "media_session" => gettext("Media Session"),
            "do_not_disturb" => gettext("Do Not Disturb"),
            other => other.to_owned(),
        }
    }
}

const fn def(id: &'static str, category: Category, icon: &'static str) -> Definition {
    Definition {
        id,
        kind: Kind::Sensor,
        category,
        icon,
        device_class: None,
        unit: None,
        state_class: None,
        entity_category: None,
    }
}

const fn percent(id: &'static str, category: Category, icon: &'static str) -> Definition {
    Definition {
        unit: Some("%"),
        state_class: Some("measurement"),
        ..def(id, category, icon)
    }
}

const fn diagnostic(definition: Definition) -> Definition {
    Definition {
        entity_category: Some("diagnostic"),
        ..definition
    }
}

const fn binary(id: &'static str, category: Category, icon: &'static str) -> Definition {
    Definition {
        kind: Kind::BinarySensor,
        ..def(id, category, icon)
    }
}

use Category::{Battery, Controls, Mobile, Network, System};

pub const DEFINITIONS: &[Definition] = &[
    Definition {
        device_class: Some("battery"),
        ..percent("battery_level", Battery, "mdi:battery")
    },
    def("battery_state", Battery, "mdi:battery-charging"),
    def("charger_type", Battery, "mdi:power-plug"),
    def("network_type", Network, "mdi:network"),
    def("wifi_connection", Network, "mdi:wifi"),
    diagnostic(def("wifi_bssid", Network, "mdi:wifi-star")),
    diagnostic(percent(
        "wifi_signal_strength",
        Network,
        "mdi:wifi-strength-3",
    )),
    diagnostic(Definition {
        device_class: Some("frequency"),
        unit: Some("MHz"),
        ..def("wifi_frequency", Network, "mdi:wifi")
    }),
    diagnostic(def("ipv4_address", Network, "mdi:ip")),
    diagnostic(def("ipv6_address", Network, "mdi:ip")),
    percent("mobile_signal_strength", Mobile, "mdi:signal-cellular-3"),
    def("mobile_operator", Mobile, "mdi:sim"),
    def("mobile_network_type", Mobile, "mdi:signal"),
    binary("screen_on", System, "mdi:cellphone"),
    binary("session_locked", System, "mdi:lock"),
    diagnostic(percent("cpu_usage", System, "mdi:cpu-64-bit")),
    diagnostic(percent("memory_usage", System, "mdi:memory")),
    diagnostic(Definition {
        device_class: Some("timestamp"),
        ..def("last_boot", System, "mdi:restart")
    }),
    percent("volume_level", Controls, "mdi:volume-high"),
    binary("volume_muted", Controls, "mdi:volume-off"),
    percent("screen_brightness", Controls, "mdi:brightness-6"),
    binary("flashlight", Controls, "mdi:flashlight"),
    def("ringer_mode", Controls, "mdi:bell-ring"),
    binary("bluetooth_state", Controls, "mdi:bluetooth"),
    Definition {
        state_class: Some("measurement"),
        ..def("bluetooth_connection", Controls, "mdi:bluetooth-connect")
    },
    def("media_session", Controls, "mdi:play-circle"),
    binary("do_not_disturb", Controls, "mdi:minus-circle"),
];

pub fn definition(id: &str) -> Option<&'static Definition> {
    DEFINITIONS.iter().find(|definition| definition.id == id)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub id: &'static str,
    pub state: Value,
    pub attributes: Map<String, Value>,
    /// Replaces the definition's icon, such as a battery icon for the level.
    pub icon: Option<String>,
}

impl Reading {
    pub fn new(id: &'static str, state: impl Into<Value>) -> Self {
        debug_assert!(definition(id).is_some(), "unknown sensor {id}");
        Self {
            id,
            state: state.into(),
            attributes: Map::new(),
            icon: None,
        }
    }

    pub fn attribute(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.attributes.insert(key.to_owned(), value.into());
        self
    }

    pub fn icon(mut self, icon: String) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn definition(&self) -> &'static Definition {
        definition(self.id).expect("readings use known IDs")
    }

    fn kind_name(&self) -> &'static str {
        match self.definition().kind {
            Kind::Sensor => "sensor",
            Kind::BinarySensor => "binary_sensor",
        }
    }

    /// `register_sensor` data. `disabled` asks Home Assistant to disable the
    /// entity, mirroring the toggle in Sanctum's settings.
    pub fn registration(&self, disabled: bool) -> Value {
        let definition = self.definition();
        let mut data = json!({
            "type": self.kind_name(),
            "unique_id": self.id,
            "name": definition.name(),
            "state": self.state,
            "icon": self.icon.as_deref().unwrap_or(definition.icon),
            "attributes": self.attributes,
            "disabled": disabled,
        });
        for (key, value) in [
            ("device_class", definition.device_class),
            ("unit_of_measurement", definition.unit),
            ("state_class", definition.state_class),
            ("entity_category", definition.entity_category),
        ] {
            if let Some(value) = value {
                data[key] = json!(value);
            }
        }
        data
    }

    /// One entry of `update_sensor_states` data.
    pub fn update(&self) -> Value {
        json!({
            "type": self.kind_name(),
            "unique_id": self.id,
            "state": self.state,
            "icon": self.icon.as_deref().unwrap_or(self.definition().icon),
            "attributes": self.attributes,
        })
    }

    /// The value as shown in settings, with its unit.
    pub fn display(&self) -> String {
        let definition = self.definition();
        let text = match &self.state {
            Value::Bool(true) => gettext("On"),
            Value::Bool(false) => gettext("Off"),
            Value::String(text) if text.is_empty() => gettext("Unknown"),
            Value::String(text) => display_text(self.id, text),
            Value::Null => gettext("Unknown"),
            other => other.to_string(),
        };
        match definition.unit {
            Some("%") => format!("{text}%"),
            Some(unit) => format!("{text} {unit}"),
            None => text,
        }
    }
}

/// Readable forms of enum-like states; names and addresses stay as they are.
fn display_text(id: &str, text: &str) -> String {
    match (id, text) {
        (_, "none") => gettext("None"),
        ("network_type", "wifi") => gettext("Wi-Fi"),
        ("charger_type" | "mobile_network_type", _) => text.replace('_', " ").to_uppercase(),
        ("battery_state" | "network_type" | "ringer_mode" | "media_session", _) => {
            crate::model::present::humanize(text)
        }
        ("last_boot", _) => glib::DateTime::from_iso8601(text, None)
            .and_then(|time| time.to_local())
            .and_then(|time| time.format("%x %X"))
            .map(|text| text.to_string())
            .unwrap_or_else(|_| text.to_owned()),
        _ => text.to_owned(),
    }
}

/// `mdi:battery-50` style icon for a level and charging state.
pub fn battery_icon(level: f64, charging: bool) -> String {
    let step = ((level / 10.0).round() as i64 * 10).clamp(0, 100);
    match (charging, step) {
        (true, 100) => "mdi:battery-charging-100".into(),
        (true, 0) => "mdi:battery-charging-outline".into(),
        (true, step) => format!("mdi:battery-charging-{step}"),
        (false, 100) => "mdi:battery".into(),
        (false, 0) => "mdi:battery-outline".into(),
        (false, step) => format!("mdi:battery-{step}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = DEFINITIONS.iter().map(|d| d.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), DEFINITIONS.len());
    }

    #[test]
    fn registration_payload() {
        let reading = Reading::new("battery_level", 57).icon(battery_icon(57.0, false));
        let data = reading.registration(false);
        assert_eq!(data["type"], "sensor");
        assert_eq!(data["device_class"], "battery");
        assert_eq!(data["unit_of_measurement"], "%");
        assert_eq!(data["icon"], "mdi:battery-60");
        assert!(data.get("entity_category").is_none());
        let data = Reading::new("screen_on", true).registration(true);
        assert_eq!(data["type"], "binary_sensor");
        assert_eq!(data["disabled"], true);
    }

    #[test]
    fn battery_icons() {
        assert_eq!(battery_icon(100.0, false), "mdi:battery");
        assert_eq!(battery_icon(3.0, false), "mdi:battery-outline");
        assert_eq!(battery_icon(44.0, true), "mdi:battery-charging-40");
    }
}
