use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use futures_channel::{mpsc, oneshot};
use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::glib::translate::{IntoGlib, ToGlibPtr};
use serde_json::{Value, json};
use soup::prelude::*;

use super::auth;
use super::credentials::{self, Credentials};
use super::endpoints::{self, Endpoints};
use super::error::{Error, Result};
use super::http;
use super::server::ServerUrl;
use super::trust::{self, CertificateInfo};

/// Seconds between reconnect attempts; the last value repeats.
const BACKOFF: [u32; 7] = [1, 2, 4, 8, 15, 30, 60];
const KEEPALIVE_SECONDS: u32 = 30;
/// Network changes arrive in bursts while an interface settles.
const RECHECK_DELAY_SECONDS: u32 = 2;
/// Full entity state dumps exceed libsoup's 128 KiB default.
const MAX_PAYLOAD_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "SanctumConnectionState")]
pub enum ConnectionState {
    /// No credentials, or stopped on purpose.
    #[default]
    Idle,
    Connecting,
    Connected,
    /// Lost or failed; a retry is scheduled.
    Disconnected,
    /// Credentials rejected; retrying stops until new credentials are set.
    AuthFailed,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    AwaitingAuthRequired,
    AwaitingAuthResult,
    Ready,
}

/// Dropping it stops local delivery; the server-side subscription stays open.
pub struct Subscription {
    pub id: u64,
    pub events: mpsc::UnboundedReceiver<Value>,
    client: glib::WeakRef<Client>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(client) = self.client.upgrade() {
            client.imp().subscriptions.borrow_mut().remove(&self.id);
        }
    }
}

mod imp {
    use std::sync::OnceLock;

    use glib::subclass::Signal;

    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Client)]
    pub struct Client {
        #[property(get, builder(ConnectionState::default()))]
        pub(super) state: Cell<ConnectionState>,
        #[property(get)]
        pub(super) error_message: RefCell<String>,
        #[property(get)]
        pub(super) ha_version: RefCell<String>,
        #[property(get)]
        pub(super) location_name: RefCell<String>,
        /// Address of the current or last connection attempt.
        #[property(get)]
        pub(super) active_url: RefCell<String>,

        pub(super) credentials: RefCell<Option<Credentials>>,
        pub(super) connection: RefCell<Option<soup::WebsocketConnection>>,
        pub(super) phase: Cell<Phase>,
        pub(super) next_id: Cell<u64>,
        pub(super) pending: RefCell<HashMap<u64, oneshot::Sender<Result<Value>>>>,
        pub(super) subscriptions: RefCell<HashMap<u64, mpsc::UnboundedSender<Value>>>,
        pub(super) retry_source: RefCell<Option<glib::SourceId>>,
        pub(super) attempt: Cell<u32>,
        /// Bumped on every connect and disconnect so callbacks from an older socket are ignored.
        pub(super) generation: Cell<u64>,
        pub(super) refresh_tried: Cell<bool>,
        pub(super) network_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) endpoints: RefCell<Endpoints>,
        pub(super) recheck_source: RefCell<Option<glib::SourceId>>,
        /// Set when the last attempt failed on a certificate the user has not accepted.
        pub(super) untrusted: RefCell<Option<CertificateInfo>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Client {
        const NAME: &'static str = "SanctumHaClient";
        type Type = super::Client;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Client {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("credentials-changed").build()])
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let monitor = gio::NetworkMonitor::default();
            let handler = monitor.connect_network_changed(glib::clone!(
                #[weak]
                obj,
                move |_, available| {
                    if available {
                        obj.recheck_soon();
                    }
                }
            ));
            self.network_handler.replace(Some(handler));
        }

        fn dispose(&self) {
            if let Some(handler) = self.network_handler.take() {
                gio::NetworkMonitor::default().disconnect(handler);
            }
            if let Some(source) = self.recheck_source.take() {
                source.remove();
            }
            self.obj().stop();
        }
    }
}

glib::wrapper! {
    pub struct Client(ObjectSubclass<imp::Client>);
}

impl Default for Client {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Client {
    pub fn credentials(&self) -> Option<Credentials> {
        self.imp().credentials.borrow().clone()
    }

    pub fn set_credentials(&self, credentials: Option<Credentials>) {
        self.stop();
        let has_credentials = credentials.is_some();
        self.imp().credentials.replace(credentials);
        if has_credentials {
            self.reconnect();
        }
    }

    pub fn connect_credentials_changed<F: Fn(&Self) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_local("credentials-changed", false, move |values| {
            let obj = values[0].get::<Self>().expect("signal emitter is a Client");
            f(&obj);
            None
        })
    }

    pub fn untrusted_certificate(&self) -> Option<CertificateInfo> {
        self.imp().untrusted.borrow().clone()
    }

    pub fn set_endpoints(&self, endpoints: Endpoints) {
        if self.imp().endpoints.replace(endpoints.clone()) != endpoints {
            self.recheck_soon();
        }
    }

    async fn choose_server(&self, credentials: &Credentials) -> ServerUrl {
        let ssids = endpoints::current_networks().await.names;
        self.imp()
            .endpoints
            .borrow()
            .choose(&credentials.server, &ssids)
    }

    /// Reconnects when the network change calls for a different address, or when disconnected.
    fn recheck_soon(&self) {
        let imp = self.imp();
        if let Some(source) = imp.recheck_source.take() {
            source.remove();
        }
        let source = glib::timeout_add_seconds_local_once(
            RECHECK_DELAY_SECONDS,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move || {
                    obj.imp().recheck_source.take();
                    glib::spawn_future_local(async move { obj.recheck().await });
                }
            ),
        );
        imp.recheck_source.replace(Some(source));
    }

    async fn recheck(&self) {
        let Some(credentials) = self.credentials() else {
            return;
        };
        match self.state() {
            ConnectionState::Disconnected => self.reconnect(),
            ConnectionState::Connected | ConnectionState::Connecting => {
                let chosen = self.choose_server(&credentials).await;
                if chosen.as_str() != self.imp().active_url.borrow().as_str() {
                    log::info!("Network changed; switching to {chosen}");
                    self.reconnect();
                }
            }
            ConnectionState::Idle | ConnectionState::AuthFailed => {}
        }
    }

    pub fn reconnect(&self) {
        let imp = self.imp();
        if imp.credentials.borrow().is_none() {
            return;
        }
        imp.untrusted.replace(None);
        self.teardown(ConnectionState::Connecting, None);
        imp.attempt.set(0);
        imp.refresh_tried.set(false);
        self.spawn_connect();
    }

    pub fn stop(&self) {
        self.teardown(ConnectionState::Idle, None);
    }

    pub async fn call(&self, mut message: Value) -> Result<Value> {
        let (sender, receiver) = oneshot::channel();
        let id = self.send_command(&mut message)?;
        self.imp().pending.borrow_mut().insert(id, sender);
        receiver.await.unwrap_or(Err(Error::NotConnected))
    }

    /// Sends a command whose `event` messages share its ID, such as
    /// `subscribe_events` or `assist_pipeline/run`.
    pub async fn subscribe(&self, mut message: Value) -> Result<Subscription> {
        let (event_sender, events) = mpsc::unbounded();
        let (sender, receiver) = oneshot::channel();
        let id = self.send_command(&mut message)?;
        self.imp()
            .subscriptions
            .borrow_mut()
            .insert(id, event_sender);
        self.imp().pending.borrow_mut().insert(id, sender);
        let subscription = Subscription {
            id,
            events,
            client: self.downgrade(),
        };
        receiver.await.unwrap_or(Err(Error::NotConnected))?;
        Ok(subscription)
    }

    /// A binary frame, used for Assist audio.
    pub fn send_binary(&self, data: &[u8]) -> Result<()> {
        let imp = self.imp();
        if imp.phase.get() != Phase::Ready {
            return Err(Error::NotConnected);
        }
        let connection = imp.connection.borrow().clone().ok_or(Error::NotConnected)?;
        // soup3-rs has no binding for this function, so it is called directly.
        // SAFETY: `connection` is a live WebsocketConnection and `data` is
        // valid for `data.len()` bytes for the duration of the call, which
        // copies it.
        unsafe {
            soup::ffi::soup_websocket_connection_send_binary(
                connection.to_glib_none().0,
                data.as_ptr().cast(),
                data.len(),
            );
        }
        Ok(())
    }

    /// Ends a subscription on the server too. For `assist_pipeline/run` this
    /// cancels the run, so speech that was not meant is never acted on.
    pub async fn unsubscribe(&self, id: u64) {
        self.imp().subscriptions.borrow_mut().remove(&id);
        let message = json!({ "type": "unsubscribe_events", "subscription": id });
        if let Err(err) = self.call(message).await {
            log::debug!("Unsubscribe {id} failed: {err}");
        }
    }

    fn send_command(&self, message: &mut Value) -> Result<u64> {
        let imp = self.imp();
        if imp.phase.get() != Phase::Ready {
            return Err(Error::NotConnected);
        }
        let connection = imp.connection.borrow().clone().ok_or(Error::NotConnected)?;
        let id = imp.next_id.get() + 1;
        imp.next_id.set(id);
        message["id"] = json!(id);
        connection.send_text(&message.to_string());
        Ok(id)
    }

    fn set_state(&self, state: ConnectionState, error: Option<String>) {
        let imp = self.imp();
        if let Some(error) = error {
            imp.error_message.replace(error);
            self.notify_error_message();
        } else if state == ConnectionState::Connected && !imp.error_message.borrow().is_empty() {
            imp.error_message.replace(String::new());
            self.notify_error_message();
        }
        if imp.state.replace(state) != state {
            self.notify_state();
        }
    }

    fn teardown(&self, state: ConnectionState, error: Option<String>) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
        if let Some(source) = imp.retry_source.take() {
            source.remove();
        }
        if let Some(connection) = imp.connection.take() {
            if connection.state() == soup::WebsocketState::Open {
                connection.close(1000, None);
            }
        }
        imp.phase.set(Phase::AwaitingAuthRequired);
        for (_, sender) in imp.pending.take() {
            let _ = sender.send(Err(Error::NotConnected));
        }
        imp.subscriptions.borrow_mut().clear();
        self.set_state(state, error);
    }

    fn spawn_connect(&self) {
        let generation = self.imp().generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                obj.connect_inner(generation).await;
            }
        ));
    }

    async fn connect_inner(&self, generation: u64) {
        let imp = self.imp();
        let Some(mut credentials) = self.credentials() else {
            return;
        };
        let server = self.choose_server(&credentials).await;
        if imp.generation.get() != generation {
            return;
        }
        imp.active_url.replace(server.to_string());
        self.notify_active_url();

        if credentials.auth.needs_refresh(auth::now()) {
            match auth::refresh(&server, &credentials.auth).await {
                Ok(refreshed) => {
                    credentials.auth = refreshed;
                    self.store_credentials(credentials.clone()).await;
                }
                Err(err) => {
                    self.fail(generation, err);
                    return;
                }
            }
        }
        if imp.generation.get() != generation {
            return;
        }

        let message = match soup::Message::new("GET", &server.websocket_url()) {
            Ok(message) => message,
            Err(_) => {
                self.fail(generation, Error::InvalidUrl(server.to_string()));
                return;
            }
        };
        trust::install(&message);
        let result = http::session()
            .websocket_connect_async_future(&message, None, &[], glib::Priority::DEFAULT)
            .await;
        if imp.generation.get() != generation {
            return;
        }
        let connection = match result {
            Ok(connection) => connection,
            Err(err) => {
                self.fail(generation, Error::from_transport(err, message.uri()));
                return;
            }
        };

        connection.set_max_incoming_payload_size(MAX_PAYLOAD_BYTES);
        connection.set_keepalive_interval(KEEPALIVE_SECONDS);
        connection.connect_message(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, kind, bytes| {
                if kind == soup::WebsocketDataType::Text.into_glib() {
                    obj.handle_text(generation, bytes);
                }
            }
        ));
        connection.connect_closed(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            move |connection| {
                if obj.imp().generation.get() == generation {
                    let reason = connection
                        .close_data()
                        .filter(|data| !data.is_empty())
                        .map(|data| data.to_string())
                        .unwrap_or_else(|| "connection closed".into());
                    obj.fail(generation, Error::Network(reason));
                }
            }
        ));
        imp.phase.set(Phase::AwaitingAuthRequired);
        imp.connection.replace(Some(connection));
    }

    fn handle_text(&self, generation: u64, bytes: &glib::Bytes) {
        let imp = self.imp();
        if imp.generation.get() != generation {
            return;
        }
        let message: Value = match serde_json::from_slice(bytes) {
            Ok(message) => message,
            Err(err) => {
                log::warn!("Ignoring malformed message: {err}");
                return;
            }
        };
        let kind = message["type"].as_str().unwrap_or_default();

        match (imp.phase.get(), kind) {
            (Phase::AwaitingAuthRequired, "auth_required") => {
                let Some(token) = self.credentials().map(|c| c.auth.access_token().to_owned())
                else {
                    return;
                };
                if let Some(connection) = imp.connection.borrow().as_ref() {
                    connection
                        .send_text(&json!({ "type": "auth", "access_token": token }).to_string());
                }
                imp.phase.set(Phase::AwaitingAuthResult);
            }
            (Phase::AwaitingAuthResult, "auth_ok") => {
                imp.phase.set(Phase::Ready);
                imp.attempt.set(0);
                imp.refresh_tried.set(false);
                imp.ha_version.replace(
                    message["ha_version"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
                self.notify_ha_version();
                self.set_state(ConnectionState::Connected, None);
                self.spawn_load_config();
            }
            (Phase::AwaitingAuthResult, "auth_invalid") => {
                let reason = message["message"]
                    .as_str()
                    .unwrap_or("invalid credentials")
                    .to_owned();
                self.fail(generation, Error::Auth(reason));
            }
            (Phase::Ready, "result") => self.handle_result(&message),
            (Phase::Ready, "event") => {
                if let Some(id) = message["id"].as_u64() {
                    if let Some(sender) = imp.subscriptions.borrow().get(&id) {
                        let _ = sender.unbounded_send(message["event"].clone());
                    }
                }
            }
            (Phase::Ready, "pong") => self.handle_result(&json!({
                "id": message["id"],
                "success": true,
                "result": null,
            })),
            (phase, kind) => log::debug!("Unhandled {kind:?} message during {phase:?}"),
        }
    }

    fn handle_result(&self, message: &Value) {
        let Some(id) = message["id"].as_u64() else {
            return;
        };
        let Some(sender) = self.imp().pending.borrow_mut().remove(&id) else {
            return;
        };
        let outcome = if message["success"].as_bool() == Some(true) {
            Ok(message["result"].clone())
        } else {
            self.imp().subscriptions.borrow_mut().remove(&id);
            Err(Error::Command {
                code: message["error"]["code"]
                    .as_str()
                    .unwrap_or("unknown_error")
                    .to_owned(),
                message: message["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
        };
        let _ = sender.send(outcome);
    }

    fn fail(&self, generation: u64, error: Error) {
        let imp = self.imp();
        if imp.generation.get() != generation {
            return;
        }
        log::info!("Connection failed: {error}");

        // Retrying cannot succeed until the user accepts the certificate.
        if let Error::UntrustedCertificate(info) = &error {
            imp.untrusted.replace(Some((**info).clone()));
            self.teardown(ConnectionState::Disconnected, Some(error.to_string()));
            return;
        }

        if let Error::Auth(_) = &error {
            let is_oauth = matches!(
                self.credentials().map(|c| c.auth),
                Some(credentials::Auth::OAuth { .. })
            );
            // An access token can be revoked before it expires, so one refresh is tried first.
            if is_oauth && !imp.refresh_tried.replace(true) {
                self.teardown(ConnectionState::Connecting, None);
                self.force_refresh_then_connect();
                return;
            }
            self.teardown(ConnectionState::AuthFailed, Some(error.to_string()));
            return;
        }

        self.teardown(ConnectionState::Disconnected, Some(error.to_string()));
        let attempt = imp.attempt.get();
        let delay = BACKOFF[(attempt as usize).min(BACKOFF.len() - 1)];
        imp.attempt.set(attempt + 1);
        let generation = imp.generation.get();
        let source = glib::timeout_add_seconds_local_once(
            delay,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move || {
                    let imp = obj.imp();
                    imp.retry_source.take();
                    if imp.generation.get() == generation {
                        obj.set_state(ConnectionState::Connecting, None);
                        obj.spawn_connect();
                    }
                }
            ),
        );
        imp.retry_source.replace(Some(source));
    }

    fn force_refresh_then_connect(&self) {
        let generation = self.imp().generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let Some(mut credentials) = obj.credentials() else {
                    return;
                };
                let server = ServerUrl::parse(&obj.active_url())
                    .unwrap_or_else(|_| credentials.server.clone());
                match auth::refresh(&server, &credentials.auth).await {
                    Ok(refreshed) => {
                        credentials.auth = refreshed;
                        obj.store_credentials(credentials).await;
                        if obj.imp().generation.get() == generation {
                            obj.connect_inner(generation).await;
                        }
                    }
                    Err(err) => obj.fail(generation, err),
                }
            }
        ));
    }

    /// An access token valid for at least another minute, refreshed first when
    /// needed. The WebSocket keeps working past expiry, but HTTP requests do not.
    pub async fn fresh_access_token(&self) -> Result<String> {
        self.fresh_token().await.map(|(token, _)| token)
    }

    /// The access token with its remaining lifetime in seconds.
    pub async fn fresh_token(&self) -> Result<(String, i64)> {
        let mut credentials = self.credentials().ok_or(Error::NotConnected)?;
        if credentials
            .auth
            .needs_refresh(glib::real_time() / 1_000_000)
        {
            let server =
                ServerUrl::parse(&self.active_url()).unwrap_or_else(|_| credentials.server.clone());
            credentials.auth = auth::refresh(&server, &credentials.auth).await?;
            self.store_credentials(credentials.clone()).await;
        }
        let now = glib::real_time() / 1_000_000;
        Ok((
            credentials.auth.access_token().to_owned(),
            credentials.auth.expires_in(now),
        ))
    }

    /// Saves a new `mobile_app` registration without reconnecting.
    pub async fn replace_registration(&self, registration: credentials::Registration) {
        self.set_registration(Some(registration)).await;
    }

    /// `None` for front-end use, where no webhook is used.
    pub async fn set_registration(&self, registration: Option<credentials::Registration>) {
        if let Some(mut credentials) = self.credentials() {
            credentials.registration = registration;
            self.store_credentials(credentials).await;
        }
    }

    async fn store_credentials(&self, credentials: Credentials) {
        if let Err(err) = credentials::store(&credentials).await {
            log::warn!("Could not save refreshed credentials: {err}");
        }
        self.imp().credentials.replace(Some(credentials));
        self.emit_by_name::<()>("credentials-changed", &[]);
    }

    fn spawn_load_config(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                match obj.call(json!({ "type": "get_config" })).await {
                    Ok(config) => {
                        let name = config["location_name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        obj.imp().location_name.replace(name);
                        obj.notify_location_name();
                    }
                    Err(err) => log::warn!("get_config failed: {err}"),
                }
            }
        ));
    }
}
