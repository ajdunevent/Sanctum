use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::error::{Error, Result};
use super::server::ServerUrl;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Auth {
    LongLivedToken {
        token: String,
    },
    #[serde(rename = "oauth")]
    OAuth {
        /// Refreshing requires the exact client ID used at sign-in, loopback port included.
        client_id: String,
        access_token: String,
        refresh_token: String,
        /// Unix seconds.
        expires_at: i64,
    },
}

impl Auth {
    pub fn access_token(&self) -> &str {
        match self {
            Self::LongLivedToken { token } => token,
            Self::OAuth { access_token, .. } => access_token,
        }
    }

    /// Long-lived tokens report a day so the frontend checks back.
    pub fn expires_in(&self, now: i64) -> i64 {
        match self {
            Self::LongLivedToken { .. } => 86_400,
            Self::OAuth { expires_at, .. } => (*expires_at - now).max(0),
        }
    }

    pub fn needs_refresh(&self, now: i64) -> bool {
        match self {
            Self::LongLivedToken { .. } => false,
            Self::OAuth { expires_at, .. } => *expires_at - 60 <= now,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Registration {
    pub webhook_id: String,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub cloudhook_url: Option<String>,
    #[serde(default)]
    pub remote_ui_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Credentials {
    pub server: ServerUrl,
    pub device_id: String,
    pub auth: Auth,
    #[serde(default)]
    pub registration: Option<Registration>,
}

const SCHEMA_NAME: &str = "net.dunevent.Sanctum.Server";
const ATTR_SERVER: &str = "server";

fn schema() -> libsecret::Schema {
    let mut attributes = HashMap::new();
    attributes.insert(ATTR_SERVER, libsecret::SchemaAttributeType::String);
    libsecret::Schema::new(SCHEMA_NAME, libsecret::SchemaFlags::NONE, attributes)
}

fn attributes(server: &ServerUrl) -> HashMap<&str, &str> {
    HashMap::from([(ATTR_SERVER, server.as_str())])
}

fn storage_error(err: gtk::glib::Error) -> Error {
    Error::Storage(err.message().to_owned())
}

pub async fn load(server: &ServerUrl) -> Result<Option<Credentials>> {
    let secret = libsecret::password_lookup_future(Some(&schema()), attributes(server))
        .await
        .map_err(storage_error)?;
    match secret {
        Some(secret) => Ok(Some(serde_json::from_str(&secret)?)),
        None => Ok(None),
    }
}

pub async fn store(credentials: &Credentials) -> Result<()> {
    let secret = serde_json::to_string(credentials)?;
    let label = format!("Sanctum: {}", credentials.server);
    libsecret::password_store_future(
        Some(&schema()),
        attributes(&credentials.server),
        None,
        &label,
        &secret,
    )
    .await
    .map_err(storage_error)
}

pub async fn clear(server: &ServerUrl) -> Result<()> {
    libsecret::password_clear_future(Some(&schema()), attributes(server))
        .await
        .map_err(storage_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth(expires_at: i64) -> Auth {
        Auth::OAuth {
            client_id: "http://127.0.0.1:40000/".into(),
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            expires_at,
        }
    }

    #[test]
    fn refresh_window() {
        assert!(!oauth(1_000).needs_refresh(900));
        assert!(oauth(1_000).needs_refresh(950));
        assert!(!Auth::LongLivedToken { token: "t".into() }.needs_refresh(i64::MAX));
    }

    #[test]
    fn credentials_round_trip() {
        let creds = Credentials {
            server: ServerUrl::parse("http://ha.local:8123").unwrap(),
            device_id: "abc".into(),
            auth: oauth(5),
            registration: Some(Registration {
                webhook_id: "hook".into(),
                secret: None,
                cloudhook_url: None,
                remote_ui_url: None,
            }),
        };
        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("\"kind\":\"oauth\""));
        assert_eq!(serde_json::from_str::<Credentials>(&json).unwrap(), creds);
    }
}
