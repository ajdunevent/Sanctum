//! How entities are shown: icon, state text, whether a tile reads as active, and
//! which section of an area page it belongs to.

use std::collections::HashSet;
use std::sync::LazyLock;

use gettextrs::{gettext, ngettext};
use gtk::glib;
use serde_json::{Map, Value};

static BUNDLED_ICONS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("../../build-aux/mdi-icons.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
});

/// Icon name for an `mdi:` reference, when that icon is bundled.
pub fn mdi_icon(reference: &str) -> Option<String> {
    let name = reference.strip_prefix("mdi:")?;
    BUNDLED_ICONS
        .contains(name)
        .then(|| format!("mdi-{name}-symbolic"))
}

fn mdi(name: &str) -> String {
    format!("mdi-{name}-symbolic")
}

/// An explicit `mdi:` icon wins when bundled; otherwise the domain default for the state.
pub fn icon_name(
    domain: &str,
    state: &str,
    device_class: Option<&str>,
    explicit: Option<&str>,
) -> String {
    if let Some(icon) = explicit.and_then(mdi_icon) {
        return icon;
    }
    let on = state == "on";
    let open = matches!(state, "open" | "opening" | "closing");
    let name = match domain {
        "light" => {
            if on {
                "lightbulb"
            } else {
                "lightbulb-outline"
            }
        }
        "switch" if device_class == Some("outlet") => {
            if on {
                "power-plug"
            } else {
                "power-plug-off"
            }
        }
        "switch" => {
            if on {
                "toggle-switch-variant"
            } else {
                "toggle-switch-variant-off"
            }
        }
        "input_boolean" => {
            if on {
                "toggle-switch-outline"
            } else {
                "toggle-switch-off-outline"
            }
        }
        "fan" => {
            if on {
                "fan"
            } else {
                "fan-off"
            }
        }
        "climate" => "thermostat",
        "water_heater" => "thermometer",
        "humidifier" => "air-humidifier",
        "cover" => match device_class {
            Some("garage") => {
                if open {
                    "garage-open"
                } else {
                    "garage"
                }
            }
            Some("door") => {
                if open {
                    "door-open"
                } else {
                    "door-closed"
                }
            }
            Some("gate") => {
                if open {
                    "gate-open"
                } else {
                    "gate"
                }
            }
            Some("window") => {
                if open {
                    "window-open"
                } else {
                    "window-closed"
                }
            }
            Some("blind" | "shade") => {
                if open {
                    "blinds-open"
                } else {
                    "blinds"
                }
            }
            Some("curtain") => {
                if open {
                    "curtains"
                } else {
                    "curtains-closed"
                }
            }
            _ => {
                if open {
                    "window-shutter-open"
                } else {
                    "window-shutter"
                }
            }
        },
        "valve" => {
            if open {
                "valve-open"
            } else {
                "valve-closed"
            }
        }
        "lock" => match state {
            "unlocked" | "unlocking" | "open" | "opening" => "lock-open-variant",
            "jammed" => "lock-alert",
            _ => "lock",
        },
        "media_player" => match device_class {
            Some("tv") => "television",
            Some("speaker") => "speaker",
            _ if state == "playing" => "cast-connected",
            _ => "cast",
        },
        "alarm_control_panel" => match state {
            "disarmed" => "shield-off",
            "armed_home" => "shield-home",
            "triggered" => "bullhorn",
            s if s.starts_with("armed") => "shield-lock",
            _ => "shield-check",
        },
        "scene" => "palette",
        "script" => "script-text",
        "automation" => {
            if on {
                "robot"
            } else {
                "robot-off"
            }
        }
        "button" | "input_button" => "gesture-tap-button",
        "camera" => "video",
        "vacuum" => "robot-vacuum",
        "siren" => "bullhorn",
        "sun" => "white-balance-sunny",
        "person" => {
            if state == "home" {
                "account"
            } else {
                "account-off"
            }
        }
        "device_tracker" => "cellphone",
        "number" | "input_number" => "ray-vertex",
        "select" | "input_select" => "format-list-bulleted",
        "text" | "input_text" => "text-box-outline",
        "update" => "package-up",
        "timer" => "timer-outline",
        "counter" => "counter",
        "calendar" => "calendar",
        "weather" => match state {
            "sunny" => "weather-sunny",
            "clear-night" => "weather-night",
            "cloudy" => "weather-cloudy",
            "rainy" => "weather-rainy",
            "pouring" => "weather-pouring",
            "snowy" | "snowy-rainy" => "weather-snowy",
            "fog" => "weather-fog",
            s if s.starts_with("lightning") => "weather-lightning",
            s if s.starts_with("windy") => "weather-windy",
            _ => "weather-partly-cloudy",
        },
        "binary_sensor" => match device_class {
            Some("motion" | "occupancy" | "presence") => {
                if on {
                    "motion-sensor"
                } else {
                    "motion-sensor-off"
                }
            }
            Some("door" | "garage_door" | "opening") => {
                if on {
                    "door-open"
                } else {
                    "door-closed"
                }
            }
            Some("window") => {
                if on {
                    "window-open"
                } else {
                    "window-closed"
                }
            }
            Some("moisture") => {
                if on {
                    "water"
                } else {
                    "water-off"
                }
            }
            Some("smoke") => "smoke-detector",
            Some("battery") => "battery-outline",
            Some("plug" | "power") => {
                if on {
                    "power-plug"
                } else {
                    "power-plug-off"
                }
            }
            _ => {
                if on {
                    "checkbox-marked-circle"
                } else {
                    "radiobox-blank"
                }
            }
        },
        "sensor" => match device_class {
            Some("temperature") => "thermometer",
            Some("humidity" | "moisture") => "water-percent",
            Some("power" | "voltage" | "current") => "flash",
            Some("energy") => "lightning-bolt",
            Some("battery") => "battery",
            Some("carbon_dioxide") => "molecule-co2",
            Some("illuminance") => "brightness-5",
            Some("pressure" | "atmospheric_pressure") => "gauge",
            _ => "eye",
        },
        _ => "eye",
    };
    mdi(name)
}

pub fn is_active(domain: &str, state: &str) -> bool {
    match domain {
        "climate" | "water_heater" => !matches!(state, "off" | "unavailable" | "unknown"),
        "media_player" => matches!(state, "playing" | "on" | "paused" | "buffering"),
        "cover" | "valve" => matches!(state, "open" | "opening" | "closing"),
        "lock" => matches!(
            state,
            "unlocked" | "unlocking" | "open" | "opening" | "jammed"
        ),
        "alarm_control_panel" => state.starts_with("armed") || state == "triggered",
        "person" | "device_tracker" => state == "home",
        "vacuum" => matches!(state, "cleaning" | "returning"),
        _ => state == "on",
    }
}

pub fn is_available(state: &str) -> bool {
    !matches!(state, "unavailable" | "unknown" | "")
}

/// "5 minutes ago" style text for a Unix time, or the date beyond a day.
pub fn ago(time: f64) -> String {
    let elapsed = (glib::real_time() / 1_000_000) - time as i64;
    let minutes = (elapsed / 60).max(0);
    let hours = minutes / 60;
    if minutes < 1 {
        gettext("just now")
    } else if hours < 1 {
        ngettext("%d minute ago", "%d minutes ago", minutes as u32).replacen(
            "%d",
            &minutes.to_string(),
            1,
        )
    } else if hours < 24 {
        ngettext("%d hour ago", "%d hours ago", hours as u32).replacen("%d", &hours.to_string(), 1)
    } else {
        glib::DateTime::from_unix_local(time as i64)
            .and_then(|when| when.format("%x"))
            .map(|date| date.to_string())
            .unwrap_or_default()
    }
}

pub fn humanize(value: &str) -> String {
    if value == "heat_cool" {
        return gettext("Heat/Cool");
    }
    let words = value.replace('_', " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

pub fn state_text(domain: &str, state: &str, attributes: &Map<String, Value>) -> String {
    match state {
        "unavailable" => return gettext("Unavailable"),
        "unknown" => return gettext("Unknown"),
        _ => {}
    }
    if let Some(unit) = attributes
        .get("unit_of_measurement")
        .and_then(Value::as_str)
    {
        let separator = if unit == "%" { "" } else { " " };
        return format!("{state}{separator}{unit}");
    }
    let text = match (domain, state) {
        (_, "on") => gettext("On"),
        (_, "off") => gettext("Off"),
        ("person" | "device_tracker", "not_home") => gettext("Away"),
        ("person" | "device_tracker", "home") => gettext("Home"),
        ("scene" | "button" | "input_button", _) => String::new(),
        _ => humanize(state),
    };
    if matches!(domain, "climate" | "water_heater") {
        if let Some(current) = attributes
            .get("current_temperature")
            .and_then(Value::as_f64)
        {
            return format!("{text} · {current}°");
        }
    }
    if domain == "light" && state == "on" {
        if let Some(brightness) = attributes.get("brightness").and_then(Value::as_f64) {
            let percent = (brightness / 255.0 * 100.0).round();
            return format!("{text} · {percent}%");
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn explicit_icon_needs_to_be_bundled() {
        assert_eq!(
            icon_name("light", "on", None, Some("mdi:fan")),
            "mdi-fan-symbolic"
        );
        assert_eq!(
            icon_name("light", "on", None, Some("mdi:not-bundled-icon")),
            "mdi-lightbulb-symbolic"
        );
        assert_eq!(
            icon_name("light", "off", None, None),
            "mdi-lightbulb-outline-symbolic"
        );
    }

    #[test]
    fn device_class_icons() {
        assert_eq!(
            icon_name("cover", "open", Some("garage"), None),
            "mdi-garage-open-symbolic"
        );
        assert_eq!(
            icon_name("cover", "closed", None, None),
            "mdi-window-shutter-symbolic"
        );
        assert_eq!(
            icon_name("sensor", "21", Some("temperature"), None),
            "mdi-thermometer-symbolic"
        );
        assert_eq!(
            icon_name("switch", "on", Some("outlet"), None),
            "mdi-power-plug-symbolic"
        );
    }

    #[test]
    fn every_default_icon_is_bundled() {
        let samples = [
            ("light", "on"),
            ("switch", "off"),
            ("fan", "on"),
            ("climate", "heat"),
            ("cover", "open"),
            ("lock", "jammed"),
            ("media_player", "playing"),
            ("alarm_control_panel", "armed_away"),
            ("scene", "x"),
            ("weather", "windy-variant"),
            ("binary_sensor", "on"),
            ("sensor", "1"),
            ("person", "home"),
            ("update", "on"),
            ("unknown_domain", "on"),
        ];
        for (domain, state) in samples {
            let icon = icon_name(domain, state, None, None);
            let name = icon
                .strip_prefix("mdi-")
                .unwrap()
                .strip_suffix("-symbolic")
                .unwrap();
            assert!(BUNDLED_ICONS.contains(name), "{icon} is not bundled");
        }
    }

    #[test]
    fn activity() {
        assert!(is_active("light", "on"));
        assert!(!is_active("light", "off"));
        assert!(is_active("climate", "heat"));
        assert!(is_active("lock", "unlocked"));
        assert!(!is_active("lock", "locked"));
        assert!(is_active("alarm_control_panel", "armed_night"));
    }

    #[test]
    fn state_texts() {
        let none = Map::new();
        assert_eq!(state_text("switch", "on", &none), "On");
        assert_eq!(state_text("cover", "opening", &none), "Opening");
        assert_eq!(state_text("person", "not_home", &none), "Away");
        let temp = json!({ "unit_of_measurement": "°C" });
        assert_eq!(
            state_text("sensor", "21.5", temp.as_object().unwrap()),
            "21.5 °C"
        );
        let pct = json!({ "unit_of_measurement": "%" });
        assert_eq!(state_text("sensor", "40", pct.as_object().unwrap()), "40%");
        let light = json!({ "brightness": 255 });
        assert_eq!(
            state_text("light", "on", light.as_object().unwrap()),
            "On · 100%"
        );
        assert_eq!(state_text("scene", "2026-10-21T10:00:00", &none), "");
        let climate = json!({ "current_temperature": 20.5 });
        assert_eq!(
            state_text("climate", "heat", climate.as_object().unwrap()),
            "Heat · 20.5°"
        );
    }
}
