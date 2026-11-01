use std::cell::OnceCell;

use gtk::glib;
use gtk::glib::translate::IntoGlib;
use serde::de::DeserializeOwned;
use soup::prelude::*;

use super::error::{Error, Result};

thread_local! {
    static SESSION: OnceCell<soup::Session> = const { OnceCell::new() };
}

pub fn session() -> soup::Session {
    SESSION.with(|cell| {
        cell.get_or_init(|| {
            let session = soup::Session::new();
            session.set_user_agent(&format!("Sanctum/{}", crate::config::VERSION));
            session.set_timeout(30);
            session
        })
        .clone()
    })
}

pub struct Response {
    pub status: u32,
    pub body: glib::Bytes,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    pub fn error_for_status(self) -> Result<Self> {
        if self.is_success() {
            Ok(self)
        } else {
            Err(Error::Http {
                status: self.status,
                body: self.text(),
            })
        }
    }
}

async fn send(message: soup::Message) -> Result<Response> {
    super::trust::install(&message);
    let body = session()
        .send_and_read_future(&message, glib::Priority::DEFAULT)
        .await
        .map_err(|err| Error::from_transport(err, message.uri()))?;
    Ok(Response {
        status: message.status().into_glib() as u32,
        body,
    })
}

fn new_message(method: &str, url: &str) -> Result<soup::Message> {
    soup::Message::new(method, url).map_err(|_| Error::InvalidUrl(url.to_owned()))
}

pub async fn post_form(url: &str, fields: &[(&str, &str)]) -> Result<Response> {
    let message = new_message("POST", url)?;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields)
        .finish();
    message.set_request_body_from_bytes(
        Some("application/x-www-form-urlencoded"),
        Some(&glib::Bytes::from_owned(body.into_bytes())),
    );
    send(message).await
}

pub async fn post_json(
    url: &str,
    bearer: Option<&str>,
    payload: &serde_json::Value,
) -> Result<Response> {
    let message = new_message("POST", url)?;
    if let Some(token) = bearer {
        set_bearer(&message, token);
    }
    let body = serde_json::to_vec(payload)?;
    message.set_request_body_from_bytes(
        Some("application/json"),
        Some(&glib::Bytes::from_owned(body)),
    );
    send(message).await
}

pub async fn get(url: &str, bearer: Option<&str>) -> Result<Response> {
    let message = new_message("GET", url)?;
    if let Some(token) = bearer {
        set_bearer(&message, token);
    }
    send(message).await
}

pub async fn delete(url: &str, bearer: Option<&str>) -> Result<Response> {
    let message = new_message("DELETE", url)?;
    if let Some(token) = bearer {
        set_bearer(&message, token);
    }
    send(message).await
}

pub fn set_bearer(message: &soup::Message, token: &str) {
    if let Some(headers) = message.request_headers() {
        headers.replace("Authorization", &format!("Bearer {token}"));
    }
}
