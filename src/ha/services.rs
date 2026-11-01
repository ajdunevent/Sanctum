use serde_json::{Value, json};

use super::client::Client;
use super::error::Result;

pub async fn call_with(
    client: &Client,
    domain: &str,
    service: &str,
    entity_id: &str,
    data: Value,
) -> Result<()> {
    client
        .call(json!({
            "type": "call_service",
            "domain": domain,
            "service": service,
            "service_data": data,
            "target": { "entity_id": entity_id },
        }))
        .await
        .map(|_| ())
}
