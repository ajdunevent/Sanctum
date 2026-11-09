use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use gtk::glib;
use serde::Deserialize;
use soup::prelude::*;

use super::credentials::Auth;
use super::error::{Error, Result};
use super::http;
use super::server::ServerUrl;

const CALLBACK_PATH: &str = "/callback";

const CALLBACK_PAGE: &str = "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width\"><title>Sanctum</title></head>\
<body style=\"font-family:sans-serif;text-align:center;padding-top:20vh\">\
<h1>Signed in</h1><p>You can close this page and return to Sanctum.</p></body></html>";

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// Browser sign-in with a loopback redirect. The client ID and redirect URI share
/// `http://127.0.0.1:<port>`, which Home Assistant accepts without fetching the
/// client ID page.
pub struct LoopbackFlow {
    server: soup::Server,
    base: ServerUrl,
    client_id: String,
    code_verifier: String,
    authorize_url: String,
    code: oneshot::Receiver<Result<String>>,
}

impl LoopbackFlow {
    pub fn start(base: &ServerUrl) -> Result<Self> {
        let server = soup::Server::builder().server_header("Sanctum").build();
        server.listen_local(0, soup::ServerListenOptions::IPV4_ONLY)?;
        let port = server
            .uris()
            .first()
            .map(glib::Uri::port)
            .filter(|port| *port > 0)
            .ok_or_else(|| Error::Network("loopback listener has no port".into()))?;

        let client_id = format!("http://127.0.0.1:{port}/");
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        let state = glib::uuid_string_random().to_string();
        let code_verifier = pkce_verifier();
        let code_challenge = pkce_challenge(&code_verifier);

        let (sender, code) = oneshot::channel();
        let sender = Rc::new(RefCell::new(Some(sender)));
        let expected_state = state.clone();
        server.add_handler(Some(CALLBACK_PATH), move |_, message, _, query| {
            if query.get("state").copied() != Some(expected_state.as_str()) {
                message.set_status(400, None);
                return;
            }
            let Some(sender) = sender.borrow_mut().take() else {
                message.set_status(410, None);
                return;
            };
            let outcome = match (query.get("code"), query.get("error")) {
                (Some(code), _) => Ok((*code).to_owned()),
                (None, Some(error)) => Err(Error::Auth((*error).to_owned())),
                (None, None) => Err(Error::Protocol("callback without code".into())),
            };
            message.set_status(200, None);
            message.set_response(
                Some("text/html; charset=utf-8"),
                soup::MemoryUse::Copy,
                CALLBACK_PAGE.as_bytes(),
            );
            let _ = sender.send(outcome);
        });

        let authorize_url = authorize_url(base, &client_id, &redirect_uri, &state, &code_challenge);

        Ok(Self {
            server,
            base: base.clone(),
            client_id,
            code_verifier,
            authorize_url,
            code,
        })
    }

    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    pub async fn finish(self) -> Result<Auth> {
        let code = self
            .code
            .await
            .map_err(|_| Error::Auth("sign-in was cancelled".into()))??;
        ServerExt::disconnect(&self.server);
        exchange_code(
            &self.base,
            &self.client_id,
            &code,
            Some(&self.code_verifier),
        )
        .await
    }
}

fn authorize_url(
    base: &ServerUrl,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .finish();
    format!("{}?{query}", base.join("/auth/authorize"))
}

/// Identifies Sanctum to Home Assistant, which lists it with the user's
/// refresh tokens.
pub const CLIENT_ID: &str = "https://dunevent.net/sanctum";
/// Sharing the client ID's host lets Home Assistant accept it without
/// fetching anything. Native sign-in receives its code directly, so nothing
/// is ever sent here.
pub const REDIRECT_URI: &str = "https://dunevent.net/sanctum/auth-callback";

pub(super) async fn exchange_code(
    base: &ServerUrl,
    client_id: &str,
    code: &str,
    code_verifier: Option<&str>,
) -> Result<Auth> {
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", client_id),
    ];
    fields.extend(code_verifier.map(|verifier| ("code_verifier", verifier)));
    let response = http::post_form(&base.join("/auth/token"), &fields).await?;
    let token = parse_token_response(response)?;
    let refresh_token = token
        .refresh_token
        .ok_or_else(|| Error::Protocol("token response without refresh_token".into()))?;
    Ok(Auth::OAuth {
        client_id: client_id.to_owned(),
        access_token: token.access_token,
        refresh_token,
        expires_at: now() + token.expires_in,
    })
}

pub async fn refresh(base: &ServerUrl, auth: &Auth) -> Result<Auth> {
    let Auth::OAuth {
        client_id,
        refresh_token,
        ..
    } = auth
    else {
        return Ok(auth.clone());
    };
    let response = http::post_form(
        &base.join("/auth/token"),
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ],
    )
    .await?;
    let token = parse_token_response(response)?;
    Ok(Auth::OAuth {
        client_id: client_id.clone(),
        access_token: token.access_token,
        refresh_token: refresh_token.clone(),
        expires_at: now() + token.expires_in,
    })
}

/// Home Assistant answers 200 even for unknown tokens, so only transport errors are logged.
pub async fn revoke(base: &ServerUrl, auth: &Auth) {
    if let Auth::OAuth { refresh_token, .. } = auth {
        if let Err(err) =
            http::post_form(&base.join("/auth/revoke"), &[("token", refresh_token)]).await
        {
            log::warn!("Token revocation failed: {err}");
        }
    }
}

/// Asks for the sign-in providers, which needs no token. A request that
/// fails authentication, such as `GET /api/` without a token, makes Home
/// Assistant post a "Login attempt failed" notification.
pub async fn probe(base: &ServerUrl) -> Result<()> {
    let response = http::get(&base.join("/auth/providers"), None).await?;
    let providers = response
        .is_success()
        .then(|| response.json::<serde_json::Value>().ok())
        .flatten();
    if providers.is_some_and(|value| value.is_array() || value.get("providers").is_some()) {
        return Ok(());
    }
    let unfinished = response
        .json::<serde_json::Value>()
        .ok()
        .and_then(|value| value.get("code").cloned())
        .is_some_and(|code| code == "onboarding_required");
    if unfinished {
        return Err(Error::Command {
            message: gettextrs::gettext("Finish setting up Home Assistant in a web browser first"),
            code: "onboarding_required".to_owned(),
        });
    }
    Err(Error::Protocol(format!(
        "{base} does not look like Home Assistant (HTTP {})",
        response.status
    )))
}

pub async fn verify_token(base: &ServerUrl, token: &str) -> Result<Auth> {
    let response = http::get(&base.join("/api/"), Some(token)).await?;
    match response.status {
        200..=299 => Ok(Auth::LongLivedToken {
            token: token.to_owned(),
        }),
        401 | 403 => Err(Error::Auth("token rejected".into())),
        _ => Err(response
            .error_for_status()
            .err()
            .unwrap_or(Error::NotConnected)),
    }
}

fn parse_token_response(response: http::Response) -> Result<TokenResponse> {
    if response.is_success() {
        return response.json();
    }
    // The token endpoint answers 400 for an invalid or revoked code or refresh token.
    if response.status == 400 || response.status == 403 {
        let detail = response
            .json::<TokenError>()
            .map(|e| {
                format!("{} {}", e.error, e.error_description)
                    .trim()
                    .to_owned()
            })
            .unwrap_or_else(|_| response.text());
        return Err(Error::Auth(detail));
    }
    Err(response
        .error_for_status()
        .err()
        .unwrap_or(Error::NotConnected))
}

pub fn now() -> i64 {
    glib::real_time() / 1_000_000
}

fn pkce_verifier() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    (0..64)
        .map(|_| {
            let index = glib::random_int_range(0, ALPHABET.len() as i32) as usize;
            ALPHABET[index] as char
        })
        .collect()
}

fn pkce_challenge(verifier: &str) -> String {
    let mut checksum =
        glib::Checksum::new(glib::ChecksumType::Sha256).expect("SHA-256 is always available");
    checksum.update(verifier.as_bytes());
    let digest = checksum.digest();
    glib::base64_encode(&digest)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_verifier_is_valid() {
        let verifier = pkce_verifier();
        assert_eq!(verifier.len(), 64);
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        );
    }
}
