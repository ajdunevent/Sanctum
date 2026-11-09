//! Native sign-in through Home Assistant's login flow API, the same steps the
//! frontend's login page takes.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::auth::{CLIENT_ID, REDIRECT_URI, exchange_code};
use super::credentials::Auth;
use super::error::Result;
use super::http;
use super::server::ServerUrl;

/// Provider types Sanctum signs in to without a browser, in order of
/// preference for the first page.
pub const NATIVE: &[&str] = &["homeassistant", "trusted_networks"];

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Provider {
    pub name: String,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
}

impl Provider {
    pub fn is_native(&self) -> bool {
        NATIVE.contains(&self.kind.as_str())
    }
}

/// The server's sign-in providers. Older servers answer with a bare list.
pub async fn providers(base: &ServerUrl) -> Result<Vec<Provider>> {
    let response = http::get(&base.join("/auth/providers"), None)
        .await?
        .error_for_status()?;
    let value = response.json::<Value>()?;
    let list = value.get("providers").cloned().unwrap_or(value);
    Ok(serde_json::from_value(list)?)
}

/// The first provider Sanctum can show natively, by `NATIVE` order.
pub fn primary(providers: &[Provider]) -> Option<&Provider> {
    NATIVE
        .iter()
        .find_map(|kind| providers.iter().find(|provider| provider.kind == *kind))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Text {
        name: String,
    },
    Select {
        name: String,
        options: Vec<(String, String)>,
    },
}

impl Field {
    pub fn name(&self) -> &str {
        match self {
            Self::Text { name } | Self::Select { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Form {
        flow_id: String,
        step_id: String,
        fields: Vec<Field>,
        /// Home Assistant's error key, such as `invalid_auth`.
        error: Option<String>,
        placeholders: HashMap<String, String>,
    },
    /// The authorization code.
    Done(String),
    Abort(String),
    /// A step Sanctum cannot show, such as a form with unknown field types
    /// or a redirect to an outside identity provider.
    Unsupported(String),
}

#[derive(Deserialize)]
struct RawStep {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    flow_id: String,
    #[serde(default)]
    step_id: String,
    #[serde(default)]
    data_schema: Vec<Value>,
    #[serde(default)]
    errors: Option<HashMap<String, String>>,
    #[serde(default)]
    description_placeholders: Option<HashMap<String, String>>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

fn field(value: &Value) -> Option<Field> {
    let name = value.get("name")?.as_str()?.to_owned();
    match value.get("type")?.as_str()? {
        "string" => Some(Field::Text { name }),
        "select" => {
            let options = value
                .get("options")?
                .as_array()?
                .iter()
                .map(|option| {
                    let pair = option.as_array()?;
                    Some((
                        pair.first()?.as_str()?.to_owned(),
                        pair.get(1)?.as_str()?.to_owned(),
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Field::Select { name, options })
        }
        _ => None,
    }
}

fn parse_step(raw: RawStep) -> Step {
    match raw.kind.as_str() {
        "form" => {
            let Some(fields) = raw
                .data_schema
                .iter()
                .map(field)
                .collect::<Option<Vec<_>>>()
            else {
                return Step::Unsupported(format!("form {}", raw.step_id));
            };
            Step::Form {
                flow_id: raw.flow_id,
                step_id: raw.step_id,
                fields,
                error: raw.errors.and_then(|errors| errors.get("base").cloned()),
                placeholders: raw.description_placeholders.unwrap_or_default(),
            }
        }
        "create_entry" => match raw.result {
            Some(code) => Step::Done(code),
            None => Step::Unsupported("create_entry without a code".into()),
        },
        "abort" => Step::Abort(raw.reason.unwrap_or_default()),
        other => Step::Unsupported(other.to_owned()),
    }
}

async fn step_response(response: http::Response) -> Result<Step> {
    let response = response.error_for_status()?;
    Ok(parse_step(response.json::<RawStep>()?))
}

/// Starts a flow without PKCE. Older servers reject the PKCE keys here, and
/// the code comes back in a response, so no redirect carries it.
pub async fn start(base: &ServerUrl, provider: &Provider) -> Result<Step> {
    let payload = serde_json::json!({
        "client_id": CLIENT_ID,
        "handler": [provider.kind, provider.id],
        "redirect_uri": REDIRECT_URI,
    });
    step_response(http::post_json(&base.join("/auth/login_flow"), None, &payload).await?).await
}

pub async fn submit(base: &ServerUrl, flow_id: &str, values: Map<String, Value>) -> Result<Step> {
    let mut payload = values;
    payload.insert("client_id".into(), CLIENT_ID.into());
    let url = base.join(&format!("/auth/login_flow/{flow_id}"));
    step_response(http::post_json(&url, None, &Value::Object(payload)).await?).await
}

pub async fn finish(base: &ServerUrl, code: &str) -> Result<Auth> {
    exchange_code(base, CLIENT_ID, code, None).await
}

/// Cancels an unfinished flow so the server forgets it.
pub async fn cancel(base: &ServerUrl, flow_id: &str) {
    let url = base.join(&format!("/auth/login_flow/{flow_id}"));
    if let Err(err) = http::delete(&url, None).await {
        log::debug!("Could not cancel the login flow: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(kind: &str) -> Provider {
        Provider {
            name: kind.to_owned(),
            id: None,
            kind: kind.to_owned(),
        }
    }

    #[test]
    fn picks_primary_by_preference() {
        let list = [
            provider("command_line"),
            provider("trusted_networks"),
            provider("homeassistant"),
        ];
        assert_eq!(primary(&list).unwrap().kind, "homeassistant");
        assert_eq!(primary(&list[..2]).unwrap().kind, "trusted_networks");
        assert!(primary(&list[..1]).is_none());
    }

    #[test]
    fn reads_steps() {
        let form: RawStep = serde_json::from_value(serde_json::json!({
            "type": "form", "flow_id": "f", "step_id": "init",
            "data_schema": [{"name": "username", "required": true, "type": "string"},
                            {"name": "password", "required": true, "type": "string"}],
            "errors": {"base": "invalid_auth"}, "description_placeholders": null
        }))
        .unwrap();
        let Step::Form { fields, error, .. } = parse_step(form) else {
            panic!("expected a form");
        };
        assert_eq!(fields.len(), 2);
        assert_eq!(error.as_deref(), Some("invalid_auth"));

        let users: RawStep = serde_json::from_value(serde_json::json!({
            "type": "form", "flow_id": "f", "step_id": "init",
            "data_schema": [{"name": "user", "type": "select", "options": [["id1", "AJ"]]}]
        }))
        .unwrap();
        let Step::Form { fields, .. } = parse_step(users) else {
            panic!("expected a form");
        };
        assert_eq!(
            fields[0],
            Field::Select {
                name: "user".into(),
                options: vec![("id1".into(), "AJ".into())]
            }
        );

        let unknown: RawStep = serde_json::from_value(serde_json::json!({
            "type": "form", "flow_id": "f", "step_id": "init",
            "data_schema": [{"name": "remember", "type": "boolean"}]
        }))
        .unwrap();
        assert!(matches!(parse_step(unknown), Step::Unsupported(_)));

        let done: RawStep =
            serde_json::from_value(serde_json::json!({"type": "create_entry", "result": "code"}))
                .unwrap();
        assert_eq!(parse_step(done), Step::Done("code".into()));

        let external: RawStep =
            serde_json::from_value(serde_json::json!({"type": "external", "url": "https://idp"}))
                .unwrap();
        assert!(matches!(parse_step(external), Step::Unsupported(_)));
    }

    #[test]
    fn reads_both_provider_formats() {
        let list = serde_json::json!([{"name": "Home Assistant Local", "id": null, "type": "homeassistant"}]);
        let wrapped = serde_json::json!({"providers": list.clone(), "preselect_remember_me": true});
        for value in [list, wrapped] {
            let inner = value.get("providers").cloned().unwrap_or(value);
            let parsed: Vec<Provider> = serde_json::from_value(inner).unwrap();
            assert_eq!(parsed[0].kind, "homeassistant");
        }
    }
}
