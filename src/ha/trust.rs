//! Certificates the user accepted for servers whose certificate the system does
//! not trust, such as self-signed ones. Each is pinned by SHA-256 fingerprint
//! per host and port.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use gio::prelude::*;
use gtk::glib;

#[derive(Clone, Debug, PartialEq)]
pub struct CertificateInfo {
    /// `host:port`.
    pub host: String,
    /// Colon-separated uppercase hex.
    pub fingerprint: String,
    pub subject: Option<String>,
    pub issuer: Option<String>,
    /// `YYYY-MM-DD`.
    pub expires: Option<String>,
    pub errors: gio::TlsCertificateFlags,
}

impl CertificateInfo {
    /// Settings entry: `host:port fingerprint`.
    pub fn entry(&self) -> String {
        format!("{} {}", self.host, self.fingerprint)
    }
}

thread_local! {
    static TRUSTED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Filled by the `accept-certificate` handler so a failed request can report what it saw.
    static REJECTED: RefCell<HashMap<String, CertificateInfo>> = RefCell::new(HashMap::new());
}

pub fn set_trusted<I: IntoIterator<Item = String>>(entries: I) {
    TRUSTED.with(|set| *set.borrow_mut() = entries.into_iter().collect());
}

pub fn trust(info: &CertificateInfo) {
    TRUSTED.with(|set| set.borrow_mut().insert(info.entry()));
}

pub fn is_trusted(info: &CertificateInfo) -> bool {
    TRUSTED.with(|set| set.borrow().contains(&info.entry()))
}

pub fn take_rejected(host: &str) -> Option<CertificateInfo> {
    REJECTED.with(|map| map.borrow_mut().remove(host))
}

pub fn host_key(uri: &glib::Uri) -> String {
    let port = match uri.port() {
        port if port > 0 => port,
        _ if uri.scheme().eq_ignore_ascii_case("http") => 80,
        _ => 443,
    };
    format!("{}:{port}", uri.host().unwrap_or_default())
}

/// The signal only fires for certificates the system rejected, so trusted CAs are unaffected.
pub fn install(message: &soup::Message) {
    message.connect_accept_certificate(|message, certificate, errors| {
        let host = message.uri().map(|uri| host_key(&uri)).unwrap_or_default();
        let info = describe(host, certificate, errors);
        if is_trusted(&info) {
            return true;
        }
        REJECTED.with(|map| map.borrow_mut().insert(info.host.clone(), info));
        false
    });
}

pub fn describe(
    host: String,
    certificate: &gio::TlsCertificate,
    errors: gio::TlsCertificateFlags,
) -> CertificateInfo {
    let der = certificate
        .certificate()
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
    CertificateInfo {
        host,
        fingerprint: fingerprint(&der),
        subject: certificate.subject_name().map(Into::into),
        issuer: certificate.issuer_name().map(Into::into),
        expires: certificate
            .not_valid_after()
            .and_then(|date| date.format("%Y-%m-%d").ok())
            .map(Into::into),
        errors,
    }
}

fn fingerprint(der: &[u8]) -> String {
    let hex = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, der)
        .map(|sum| sum.to_uppercase())
        .unwrap_or_default();
    hex.as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(host: &str, fingerprint: &str) -> CertificateInfo {
        CertificateInfo {
            host: host.into(),
            fingerprint: fingerprint.into(),
            subject: None,
            issuer: None,
            expires: None,
            errors: gio::TlsCertificateFlags::UNKNOWN_CA,
        }
    }

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(b"abc");
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert!(fp.starts_with("BA:78:16:BF"));
    }

    #[test]
    fn trust_is_per_host_and_fingerprint() {
        set_trusted(["ha.local:8123 AA:BB".to_owned()]);
        assert!(is_trusted(&info("ha.local:8123", "AA:BB")));
        assert!(!is_trusted(&info("ha.local:8123", "AA:CC")));
        assert!(!is_trusted(&info("other.local:8123", "AA:BB")));
        trust(&info("other.local:8123", "AA:BB"));
        assert!(is_trusted(&info("other.local:8123", "AA:BB")));
    }

    #[test]
    fn host_key_defaults_port() {
        let uri = glib::Uri::parse("https://ha.example.com/api/", glib::UriFlags::NONE).unwrap();
        assert_eq!(host_key(&uri), "ha.example.com:443");
        let uri = glib::Uri::parse("https://10.0.0.2:8123/api/", glib::UriFlags::NONE).unwrap();
        assert_eq!(host_key(&uri), "10.0.0.2:8123");
    }
}
