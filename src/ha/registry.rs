//! The entity registry, reduced to what detail sheets need: user-chosen icons
//! and hidden entities.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::client::Client;
use super::error::Result;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityEntry {
    pub icon: Option<String>,
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registries {
    pub entities: HashMap<String, EntityEntry>,
}

pub async fn load(client: &Client) -> Result<Registries> {
    let entities = client
        .call(json!({ "type": "config/entity_registry/list_for_display" }))
        .await?;
    Ok(Registries {
        entities: parse_display_entities(&entities),
    })
}

fn parse_display_entities(result: &Value) -> HashMap<String, EntityEntry> {
    result["entities"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            Some((
                entry["ei"].as_str()?.to_owned(),
                EntityEntry {
                    icon: entry["ic"].as_str().map(str::to_owned),
                    hidden: entry["hb"].as_bool().unwrap_or(false),
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_display_entities() {
        let result = json!({
            "entities": [
                { "ei": "light.porch", "pl": "hue", "ic": "mdi:outdoor-lamp" },
                { "ei": "switch.hidden", "pl": "hue", "hb": true }
            ]
        });
        let entries = parse_display_entities(&result);
        assert_eq!(
            entries["light.porch"].icon.as_deref(),
            Some("mdi:outdoor-lamp")
        );
        assert!(!entries["light.porch"].hidden);
        assert!(entries["switch.hidden"].hidden);
    }
}
