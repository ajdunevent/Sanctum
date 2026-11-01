//! The `mobile_app` webhook, used for sensors, location and notification
//! action events. Requests carry no token; the webhook ID is the credential.

use serde_json::{Value, json};

use super::client::Client;
use super::error::{Error, Result};
use super::http;

/// Status Home Assistant returns once the device was deleted there.
const GONE: u32 = 410;

/// Posts `{"type": kind, "data": data}` to the webhook on the active address.
pub async fn send(client: &Client, kind: &str, data: Value) -> Result<Value> {
    let registration = client
        .credentials()
        .and_then(|credentials| credentials.registration)
        .ok_or(Error::NotConnected)?;
    let base = client.active_url();
    if base.is_empty() {
        return Err(Error::NotConnected);
    }
    let url = format!(
        "{}/api/webhook/{}",
        base.trim_end_matches('/'),
        registration.webhook_id
    );
    let response = http::post_json(&url, None, &json!({ "type": kind, "data": data })).await?;
    match response.status {
        GONE => Err(Error::RegistrationGone),
        _ => {
            let response = response.error_for_status()?;
            if response.body.is_empty() {
                Ok(Value::Null)
            } else {
                response.json()
            }
        }
    }
}

/// Unique IDs the server reported as unknown in an `update_sensor_states`
/// reply, which happens after the sensor was deleted in Home Assistant.
pub fn unregistered(reply: &Value) -> Vec<String> {
    reply
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, result)| result["error"]["code"] == "not_registered")
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_unregistered_sensors() {
        let reply = json!({
            "battery_level": { "success": true },
            "wifi_bssid": { "success": false, "error": { "code": "not_registered", "message": "" } }
        });
        assert_eq!(unregistered(&reply), vec!["wifi_bssid".to_owned()]);
        assert!(unregistered(&Value::Null).is_empty());
    }
}
