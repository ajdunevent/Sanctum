use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};

use crate::ha::trust::CertificateInfo;

fn problems(errors: gio::TlsCertificateFlags) -> Vec<String> {
    [
        (
            gio::TlsCertificateFlags::UNKNOWN_CA,
            gettext("Not issued by a trusted authority, for example self-signed"),
        ),
        (
            gio::TlsCertificateFlags::BAD_IDENTITY,
            gettext("Issued for a different address"),
        ),
        (
            gio::TlsCertificateFlags::NOT_ACTIVATED,
            gettext("Not valid yet"),
        ),
        (gio::TlsCertificateFlags::EXPIRED, gettext("Expired")),
        (gio::TlsCertificateFlags::REVOKED, gettext("Revoked")),
        (
            gio::TlsCertificateFlags::INSECURE,
            gettext("Uses an insecure algorithm"),
        ),
        (
            gio::TlsCertificateFlags::GENERIC_ERROR,
            gettext("Could not be verified"),
        ),
    ]
    .into_iter()
    .filter(|(flag, _)| errors.contains(*flag))
    .map(|(_, text)| text)
    .collect()
}

/// Four lines of 8 bytes, since a colon-separated fingerprint has no break points.
fn split_fingerprint(fingerprint: &str) -> String {
    let parts: Vec<&str> = fingerprint.split(':').collect();
    parts
        .chunks(8)
        .map(|chunk| chunk.join(":"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn confirm_trust(parent: &impl IsA<gtk::Widget>, info: &CertificateInfo) -> bool {
    let escape = |text: &str| glib::markup_escape_text(text).to_string();
    let mut body = gettext("{} presented a certificate this device does not trust.")
        .replace("{}", &format!("<b>{}</b>", escape(&info.host)));
    for problem in problems(info.errors) {
        body.push_str(&format!("\n• {}", escape(&problem)));
    }
    let details = [
        (gettext("Issued to"), info.subject.as_deref()),
        (gettext("Issued by"), info.issuer.as_deref()),
        (gettext("Expires"), info.expires.as_deref()),
    ];
    body.push('\n');
    for (label, value) in details {
        if let Some(value) = value {
            body.push_str(&format!("\n{}: {}", escape(&label), escape(value)));
        }
    }
    body.push_str(&format!(
        "\n\n{}\n<tt>{}</tt>\n\n{}",
        escape(&gettext("SHA-256 fingerprint:")),
        escape(&split_fingerprint(&info.fingerprint)),
        escape(&gettext(
            "Trust it only if this fingerprint matches your server's certificate."
        )),
    ));

    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Untrusted Certificate"))
        .body(body)
        .body_use_markup(true)
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("trust", &gettext("_Trust Certificate")),
    ]);
    dialog.set_response_appearance("trust", adw::ResponseAppearance::Destructive);
    dialog.choose_future(Some(parent)).await == "trust"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_splits_into_four_lines() {
        let fp = (0..32)
            .map(|i| format!("{i:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let split = split_fingerprint(&fp);
        let lines: Vec<&str> = split.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].ends_with(":07"));
        assert!(lines[1].starts_with("08:"));
    }
}
