use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use url::Url;

use super::error::{Error, Result};

/// Normalized Home Assistant base URL: scheme, host, optional port and path prefix,
/// with no trailing slash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServerUrl(Url);

impl ServerUrl {
    /// Parses user input. Without a scheme, local-looking hosts (`.local`, IP
    /// addresses, explicit ports) get `http`; anything else gets `https`.
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(Error::InvalidUrl(input.to_owned()));
        }

        let with_scheme = if input.contains("://") {
            input.to_owned()
        } else {
            let probe = Url::parse(&format!("http://{input}"))
                .map_err(|e| Error::InvalidUrl(e.to_string()))?;
            let scheme = if looks_local(&probe) { "http" } else { "https" };
            format!("{scheme}://{input}")
        };

        let mut url = Url::parse(&with_scheme).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::InvalidUrl(format!(
                "unsupported scheme {}",
                url.scheme()
            )));
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(Error::InvalidUrl(input.to_owned()));
        }
        url.set_query(None);
        url.set_fragment(None);
        let trimmed = url.path().trim_end_matches('/').to_owned();
        url.set_path(&trimmed);
        Ok(Self(url))
    }

    /// Addresses to try for what the user typed, most secure first. `http://`
    /// and a missing scheme try `https://` first. Each candidate says whether
    /// the user asked for it, which decides if a certificate prompt is fair.
    pub fn candidates(input: &str) -> Result<Vec<(Self, bool)>> {
        let input = input.trim();
        let lower = input.to_ascii_lowercase();
        if lower.starts_with("https://") {
            return Ok(vec![(Self::parse(input)?, true)]);
        }
        let (rest, typed_http) = match lower.strip_prefix("http://") {
            Some(_) => (&input["http://".len()..], true),
            None if input.contains("://") => return Ok(vec![(Self::parse(input)?, true)]),
            None => (input, false),
        };
        Ok(vec![
            (Self::parse(&format!("https://{rest}"))?, !typed_http),
            (Self::parse(&format!("http://{rest}"))?, true),
        ])
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str().trim_end_matches('/')
    }

    /// `path` starts with `/`.
    pub fn join(&self, path: &str) -> String {
        format!("{}{}", self.as_str(), path)
    }

    pub fn websocket_url(&self) -> String {
        let base = self.join("/api/websocket");
        if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            base
        }
    }

    pub fn host(&self) -> &str {
        self.0.host_str().unwrap_or_default()
    }

    /// True for addresses that only resolve or route on a local network.
    pub fn is_private(&self) -> bool {
        let host = self.host();
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        match bare.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
            Ok(IpAddr::V6(ip)) => {
                ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
            }
            Err(_) => {
                !host.contains('.')
                    || [".local", ".lan", ".home", ".internal", ".home.arpa"]
                        .iter()
                        .any(|suffix| host.ends_with(suffix))
            }
        }
    }
}

fn looks_local(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    url.port().is_some()
        || host.ends_with(".local")
        || host == "localhost"
        || bare.parse::<IpAddr>().is_ok()
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for ServerUrl {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl From<ServerUrl> for String {
    fn from(value: ServerUrl) -> Self {
        value.as_str().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hosts_default_to_http() {
        assert_eq!(
            ServerUrl::parse("homeassistant.local:8123")
                .unwrap()
                .as_str(),
            "http://homeassistant.local:8123"
        );
        assert_eq!(
            ServerUrl::parse("192.168.1.20:8123").unwrap().as_str(),
            "http://192.168.1.20:8123"
        );
        assert_eq!(
            ServerUrl::parse("homeassistant.local").unwrap().as_str(),
            "http://homeassistant.local"
        );
    }

    #[test]
    fn candidates_prefer_https() {
        let names = |input: &str| -> Vec<(String, bool)> {
            ServerUrl::candidates(input)
                .unwrap()
                .into_iter()
                .map(|(url, asked)| (url.as_str().to_owned(), asked))
                .collect()
        };
        assert_eq!(
            names("ha.local:8123"),
            [
                ("https://ha.local:8123".to_owned(), true),
                ("http://ha.local:8123".to_owned(), true)
            ]
        );
        assert_eq!(
            names("HTTP://ha.local:8123/"),
            [
                ("https://ha.local:8123".to_owned(), false),
                ("http://ha.local:8123".to_owned(), true)
            ]
        );
        assert_eq!(
            names("https://ha.example.com"),
            [("https://ha.example.com".to_owned(), true)]
        );
        assert!(ServerUrl::candidates("ftp://ha").is_err());
    }

    #[test]
    fn public_hosts_default_to_https() {
        assert_eq!(
            ServerUrl::parse("ha.example.com").unwrap().as_str(),
            "https://ha.example.com"
        );
    }

    #[test]
    fn explicit_scheme_is_kept_and_trailing_slash_removed() {
        let url = ServerUrl::parse(" http://ha.example.com/ ").unwrap();
        assert_eq!(url.as_str(), "http://ha.example.com");
        assert_eq!(url.join("/auth/token"), "http://ha.example.com/auth/token");
    }

    #[test]
    fn path_prefix_survives() {
        let url = ServerUrl::parse("https://example.com/ha/").unwrap();
        assert_eq!(url.websocket_url(), "wss://example.com/ha/api/websocket");
    }

    #[test]
    fn websocket_scheme_follows_http_scheme() {
        let url = ServerUrl::parse("http://10.0.0.5:8123").unwrap();
        assert_eq!(url.websocket_url(), "ws://10.0.0.5:8123/api/websocket");
    }

    #[test]
    fn rejects_garbage() {
        assert!(ServerUrl::parse("").is_err());
        assert!(ServerUrl::parse("ftp://example.com").is_err());
        assert!(ServerUrl::parse("http://").is_err());
    }

    #[test]
    fn serde_round_trip() {
        let url = ServerUrl::parse("https://ha.example.com").unwrap();
        let json = serde_json::to_string(&url).unwrap();
        assert_eq!(json, "\"https://ha.example.com\"");
        assert_eq!(serde_json::from_str::<ServerUrl>(&json).unwrap(), url);
    }

    #[test]
    fn recognizes_private_addresses() {
        for url in [
            "http://192.168.1.5:8123",
            "http://10.0.0.2",
            "http://homeassistant.local:8123",
            "http://homeassistant:8123",
            "http://[fd00::5]:8123",
        ] {
            assert!(ServerUrl::parse(url).unwrap().is_private(), "{url}");
        }
        for url in [
            "https://ha.example.com",
            "https://8.8.8.8",
            "https://x.ui.nabu.casa",
        ] {
            assert!(!ServerUrl::parse(url).unwrap().is_private(), "{url}");
        }
    }
}
