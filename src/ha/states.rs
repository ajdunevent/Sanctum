//! `subscribe_entities` messages. Each event holds additions (`a`), changes (`c`)
//! with `+`/`-` diffs, and removals (`r`), using compressed keys: `s` state,
//! `a` attributes, `lc` last changed, `lu` last updated, `c` context.

use std::collections::HashMap;

use serde_json::{Map, Value};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityState {
    pub state: String,
    pub attributes: Map<String, Value>,
    /// Unix seconds.
    pub last_changed: f64,
}

impl EntityState {
    fn from_compressed(value: &Value) -> Self {
        let last_changed = value["lc"].as_f64().unwrap_or_default();
        Self {
            state: value["s"].as_str().unwrap_or_default().to_owned(),
            attributes: value["a"].as_object().cloned().unwrap_or_default(),
            last_changed: if last_changed > 0.0 {
                last_changed
            } else {
                value["lu"].as_f64().unwrap_or_default()
            },
        }
    }

    fn apply_diff(&mut self, diff: &Value) {
        let additions = &diff["+"];
        if let Some(state) = additions["s"].as_str() {
            self.state = state.to_owned();
        }
        if let Some(changed) = additions["lc"].as_f64() {
            self.last_changed = changed;
        }
        if let Some(attributes) = additions["a"].as_object() {
            for (key, value) in attributes {
                self.attributes.insert(key.clone(), value.clone());
            }
        }
        if let Some(removed) = diff["-"]["a"].as_array() {
            for key in removed.iter().filter_map(Value::as_str) {
                self.attributes.remove(key);
            }
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Update {
    /// Entity IDs whose state changed, in no particular order.
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// A change for an unknown entity is ignored.
pub fn apply(states: &mut HashMap<String, EntityState>, event: &Value) -> Update {
    let mut update = Update::default();
    if let Some(added) = event["a"].as_object() {
        for (entity_id, value) in added {
            let state = EntityState::from_compressed(value);
            if states.insert(entity_id.clone(), state).is_some() {
                update.changed.push(entity_id.clone());
            } else {
                update.added.push(entity_id.clone());
            }
        }
    }
    if let Some(changes) = event["c"].as_object() {
        for (entity_id, diff) in changes {
            if let Some(state) = states.get_mut(entity_id) {
                state.apply_diff(diff);
                update.changed.push(entity_id.clone());
            }
        }
    }
    if let Some(removed) = event["r"].as_array() {
        for entity_id in removed.iter().filter_map(Value::as_str) {
            if states.remove(entity_id).is_some() {
                update.removed.push(entity_id.to_owned());
            }
        }
    }
    update
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn initial_event_adds_everything() {
        let mut states = HashMap::new();
        let update = apply(
            &mut states,
            &json!({ "a": {
                "light.kitchen": { "s": "on", "a": { "brightness": 200 }, "lc": 10.0 },
                "sensor.temp": { "s": "21.5", "a": {}, "lc": 0, "lu": 12.5 }
            }}),
        );
        assert_eq!(update.added.len(), 2);
        assert_eq!(states["light.kitchen"].state, "on");
        assert_eq!(states["light.kitchen"].attributes["brightness"], 200);
        assert_eq!(states["sensor.temp"].last_changed, 12.5);
    }

    #[test]
    fn diff_updates_and_removes_attributes() {
        let mut states = HashMap::new();
        apply(
            &mut states,
            &json!({ "a": { "light.kitchen": { "s": "on", "a": { "brightness": 200, "color_mode": "hs" } } } }),
        );
        let update = apply(
            &mut states,
            &json!({ "c": { "light.kitchen": {
                "+": { "s": "off", "lc": 20.0, "a": { "friendly_name": "Kitchen" } },
                "-": { "a": ["brightness", "color_mode"] }
            }}}),
        );
        assert_eq!(update.changed, vec!["light.kitchen"]);
        let state = &states["light.kitchen"];
        assert_eq!(state.state, "off");
        assert_eq!(state.last_changed, 20.0);
        assert_eq!(
            state.attributes.get("friendly_name"),
            Some(&json!("Kitchen"))
        );
        assert!(!state.attributes.contains_key("brightness"));
    }

    #[test]
    fn removal_and_unknown_change() {
        let mut states = HashMap::new();
        apply(
            &mut states,
            &json!({ "a": { "switch.fan": { "s": "on" } } }),
        );
        let update = apply(
            &mut states,
            &json!({ "c": { "switch.ghost": { "+": { "s": "on" } } }, "r": ["switch.fan"] }),
        );
        assert!(update.changed.is_empty());
        assert_eq!(update.removed, vec!["switch.fan"]);
        assert!(states.is_empty());
    }
}
