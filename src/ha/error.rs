use std::fmt;

use super::trust::CertificateInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Transport failure: DNS, TLS, refused connection, dropped socket.
    Network(String),
    /// Credentials were rejected or a refresh token was revoked.
    Auth(String),
    Http {
        status: u32,
        body: String,
    },
    /// WebSocket command answered with `success: false`.
    Command {
        code: String,
        message: String,
    },
    /// Response that does not match the documented API shape.
    Protocol(String),
    NotConnected,
    InvalidUrl(String),
    Storage(String),
    /// TLS failed on a certificate the user has not accepted.
    UntrustedCertificate(Box<CertificateInfo>),
    /// The device was deleted in Home Assistant, so the webhook no longer exists.
    RegistrationGone,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Auth(msg) => write!(f, "authentication failed: {msg}"),
            Self::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            Self::Command { code, message } => write!(f, "{code}: {message}"),
            Self::Protocol(msg) => write!(f, "unexpected response: {msg}"),
            Self::NotConnected => write!(f, "not connected"),
            Self::InvalidUrl(msg) => write!(f, "invalid address: {msg}"),
            Self::Storage(msg) => write!(f, "keyring error: {msg}"),
            Self::RegistrationGone => write!(f, "device registration was deleted"),
            Self::UntrustedCertificate(info) => {
                write!(
                    f,
                    "untrusted certificate for {} ({})",
                    info.host, info.fingerprint
                )
            }
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Converts a transport error, reporting a rejected certificate for `uri` when there was one.
    pub fn from_transport(err: gtk::glib::Error, uri: Option<gtk::glib::Uri>) -> Self {
        uri.map(|uri| super::trust::host_key(&uri))
            .and_then(|host| super::trust::take_rejected(&host))
            .map_or_else(
                || err.into(),
                |info| Self::UntrustedCertificate(Box::new(info)),
            )
    }
}

impl From<gtk::glib::Error> for Error {
    fn from(err: gtk::glib::Error) -> Self {
        Self::Network(err.message().to_owned())
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::Protocol(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
