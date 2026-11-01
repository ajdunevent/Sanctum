use serde_json::{Value, json};

use super::client::Client;
use super::credentials::Registration;
use super::device::DeviceInfo;
use super::error::{Error, Result};
use super::http;
use super::server::ServerUrl;

/// Registers this install with the `mobile_app` integration, which creates the
/// device in Home Assistant and returns the webhook used for sensor updates.
pub async fn register(
    base: &ServerUrl,
    access_token: &str,
    device_id: &str,
    device: &DeviceInfo,
) -> Result<Registration> {
    let payload = json!({
        "device_id": device_id,
        "app_id": crate::config::BASE_ID,
        "app_name": "Sanctum",
        "app_version": crate::config::VERSION,
        "device_name": device.name,
        "manufacturer": device.manufacturer,
        "model": device.model,
        "os_name": device.os_name,
        "os_version": device.os_version,
        "supports_encryption": false,
        "app_data": {
            "push_websocket_channel": true,
        },
    });
    let response = http::post_json(
        &base.join("/api/mobile_app/registrations"),
        Some(access_token),
        &payload,
    )
    .await?;
    match response.status {
        200..=299 => response.json(),
        401 | 403 => Err(Error::Auth("registration rejected".into())),
        // The integration is part of default_config; a 404 means it is not loaded.
        404 => Err(Error::Http {
            status: 404,
            body: "mobile_app integration is not enabled".into(),
        }),
        _ => Err(response
            .error_for_status()
            .err()
            .unwrap_or(Error::NotConnected)),
    }
}

/// The config entries behind this device in the device registry. Mobile App
/// devices are identified by the `device_id` sent at registration.
fn entries_for(devices: &Value, device_id: &str) -> Vec<String> {
    devices
        .as_array()
        .into_iter()
        .flatten()
        .filter(|device| {
            device["identifiers"].as_array().is_some_and(|ids| {
                ids.iter()
                    .any(|id| id[0] == "mobile_app" && id[1] == device_id)
            })
        })
        .flat_map(|device| {
            device["config_entries"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|entry| entry.as_str().map(str::to_owned))
        .collect()
}

/// Deletes this device from Home Assistant, which only administrators may
/// do. `Ok(false)` when Home Assistant has no such device.
pub async fn remove(client: &Client, device_id: &str) -> Result<bool> {
    let devices = client
        .call(json!({ "type": "config/device_registry/list" }))
        .await?;
    let entries = entries_for(&devices, device_id);
    if entries.is_empty() {
        return Ok(false);
    }
    let token = client.fresh_access_token().await?;
    let base = client.active_url();
    for entry in entries {
        let url = format!(
            "{}/api/config/config_entries/entry/{entry}",
            base.trim_end_matches('/')
        );
        let response = http::delete(&url, Some(&token)).await?;
        if matches!(response.status, 401 | 403) {
            return Err(Error::Auth(
                "only an administrator can remove devices".into(),
            ));
        }
        response.error_for_status()?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_config_entries() {
        let devices = json!([
            { "identifiers": [["mobile_app", "abc"]], "config_entries": ["e1"] },
            { "identifiers": [["zha", "abc"]], "config_entries": ["e2"] },
            { "identifiers": [["mobile_app", "other"]], "config_entries": ["e3"] }
        ]);
        assert_eq!(entries_for(&devices, "abc"), vec!["e1".to_owned()]);
        assert!(entries_for(&devices, "missing").is_empty());
    }
}
