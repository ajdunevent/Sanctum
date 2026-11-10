//! Home Assistant notifications over the `mobile_app` WebSocket push channel,
//! shown with GNotification. Action buttons fire
//! `mobile_app_notification_action` events back through the webhook.

use std::cell::Cell;
use std::rc::Rc;

use futures_util::StreamExt;
use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use serde_json::{Value, json};

use crate::ha::{self, ConnectionState, webhook};

/// Notification IDs are namespaced so tags cannot collide with other IDs the
/// app might use.
const ID_PREFIX: &str = "ha-";
/// The portal shows at most three buttons on most desktops.
const MAX_BUTTONS: usize = 3;
/// Images larger than this are skipped; the portal passes icons in memory.
const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
const CONNECT_WAIT_STEP: std::time::Duration = std::time::Duration::from_millis(250);
/// 30 seconds in total.
const CONNECT_WAIT_STEPS: u32 = 120;

/// What a click or button does, carried as the action's target.
#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    /// Opens the main window.
    App,
    /// An entity's details in Sanctum, from `entityId:light.kitchen`.
    Entity(String),
    /// A dashboard path or full URL, opened in the browser.
    Url(String),
    /// `noAction`: the click does nothing.
    Nothing,
}

impl Click {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") => Self::App,
            Some("noAction") => Self::Nothing,
            Some(value) => match value.strip_prefix("entityId:") {
                Some(entity) => Self::Entity(entity.to_owned()),
                None => Self::Url(value.to_owned()),
            },
        }
    }

    /// Serialized for the `app.open-click` action target.
    pub fn target(&self) -> String {
        match self {
            Self::App => String::new(),
            Self::Entity(id) => format!("entityId:{id}"),
            Self::Url(url) => url.clone(),
            Self::Nothing => "noAction".into(),
        }
    }
}

/// One `actions` entry of a notification.
#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub action: String,
    pub title: String,
    /// Set for `URI` actions; such buttons open the link and fire no event.
    pub uri: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Push {
    Show(Box<Notice>),
    Clear { tag: String },
    RequestLocation,
    Command(crate::control::Command),
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub title: String,
    pub message: String,
    pub tag: Option<String>,
    pub priority: gio::NotificationPriority,
    pub click: Click,
    pub buttons: Vec<Button>,
    pub image: Option<String>,
}

impl Notice {
    /// Tagged notifications replace earlier ones with the same tag.
    pub fn id(&self, fallback: u64) -> String {
        match &self.tag {
            Some(tag) => format!("{ID_PREFIX}{tag}"),
            None => format!("{ID_PREFIX}#{fallback}"),
        }
    }
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Interprets one push event as the mobile apps do, using the Android field
/// names with the iOS equivalents as fallbacks.
pub fn parse(event: &Value) -> Push {
    let message = text(&event["message"]).unwrap_or_default();
    let data = &event["data"];
    let tag = text(&data["tag"]);
    match message.as_str() {
        "clear_notification" => {
            return match tag {
                Some(tag) => Push::Clear { tag },
                None => Push::Unsupported(message),
            };
        }
        "request_location_update" => return Push::RequestLocation,
        _ => {}
    }
    if let Some(command) = crate::control::parse(&message, data) {
        return Push::Command(command);
    }

    let level = data["priority"]
        .as_str()
        .or(data["importance"].as_str())
        .or(data["push"]["interruption-level"].as_str());
    let priority = match level {
        Some("critical") => gio::NotificationPriority::Urgent,
        Some("high" | "max" | "time-sensitive") => gio::NotificationPriority::High,
        Some("low" | "min" | "passive") => gio::NotificationPriority::Low,
        _ => gio::NotificationPriority::Normal,
    };
    let click = Click::parse(data["clickAction"].as_str().or(data["url"].as_str()));
    let buttons = data["actions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|button| {
            let action = text(&button["action"])?;
            let title = text(&button["title"]).unwrap_or_else(|| action.clone());
            let uri = (action == "URI").then(|| text(&button["uri"])).flatten();
            Some(Button { action, title, uri })
        })
        // Text replies need an input field GNotification does not offer.
        .filter(|button| button.action != "REPLY")
        .take(MAX_BUTTONS)
        .collect();
    let image = text(&data["image"]).or_else(|| text(&data["attachment"]["url"]));
    Push::Show(Box::new(Notice {
        title: text(&event["title"]).unwrap_or_else(|| "Home Assistant".into()),
        message,
        tag,
        priority,
        click,
        buttons,
        image,
    }))
}

/// `mobile_app_notification_action` event data for a pressed button.
pub fn action_event(action: &str, tag: &str, title: &str, message: &str) -> Value {
    let mut data = json!({ "action": action, "title": title, "message": message });
    if !tag.is_empty() {
        data["tag"] = json!(tag);
    }
    json!({ "event_type": "mobile_app_notification_action", "event_data": data })
}

pub struct Notifier {
    app: glib::WeakRef<gio::Application>,
    client: ha::Client,
    started: Cell<bool>,
    paused: Cell<bool>,
    counter: Cell<u64>,
    /// Bumped per connection; a subscription loop from an older one stops.
    generation: Cell<u64>,
}

impl Notifier {
    pub fn new(app: &impl IsA<gio::Application>, client: &ha::Client) -> Rc<Self> {
        Rc::new(Self {
            app: app.upcast_ref::<gio::Application>().downgrade(),
            client: client.clone(),
            started: Cell::new(false),
            paused: Cell::new(false),
            counter: Cell::new(0),
            generation: Cell::new(0),
        })
    }

    pub fn start(self: &Rc<Self>) {
        if self.started.replace(true) {
            if self.paused.replace(false) {
                self.on_connection(self.client.state());
            }
            return;
        }
        let weak = Rc::downgrade(self);
        self.client.connect_state_notify(move |client| {
            if let Some(notifier) = weak.upgrade() {
                notifier.on_connection(client.state());
            }
        });
        self.on_connection(self.client.state());
    }

    /// For front-end use: notifications stop until `start`. The open
    /// subscription ends with the next message it brings.
    pub fn pause(&self) {
        self.paused.set(true);
        self.generation.set(self.generation.get() + 1);
    }

    /// After a new registration, whose webhook ID differs.
    pub fn resubscribe(self: &Rc<Self>) {
        if self.started.get() {
            self.on_connection(self.client.state());
        }
    }

    fn on_connection(self: &Rc<Self>, state: ConnectionState) {
        self.generation.set(self.generation.get() + 1);
        if state != ConnectionState::Connected || self.paused.get() {
            return;
        }
        let generation = self.generation.get();
        let notifier = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(this) = notifier.upgrade() else {
                return;
            };
            this.listen(generation).await;
        });
    }

    async fn listen(self: &Rc<Self>, generation: u64) {
        let Some(webhook_id) = self
            .client
            .credentials()
            .and_then(|credentials| credentials.registration)
            .map(|registration| registration.webhook_id)
        else {
            return;
        };
        let subscription = self
            .client
            .subscribe(json!({
                "type": "mobile_app/push_notification_channel",
                "webhook_id": webhook_id,
                "support_confirm": true,
            }))
            .await;
        let mut subscription = match subscription {
            Ok(subscription) => subscription,
            Err(ha::Error::Command { code, .. }) if code == "not_found" => {
                crate::ui::app().registration_lost();
                return;
            }
            Err(err) => {
                log::warn!("Could not subscribe to notifications: {err}");
                return;
            }
        };
        while let Some(event) = subscription.events.next().await {
            if self.generation.get() != generation {
                return;
            }
            if let Some(confirm_id) = event["hass_confirm_id"].as_str() {
                let client = self.client.clone();
                let message = json!({
                    "type": "mobile_app/push_notification_confirm",
                    "webhook_id": webhook_id,
                    "confirm_id": confirm_id,
                });
                glib::spawn_future_local(async move {
                    if let Err(err) = client.call(message).await {
                        log::info!("Notification confirm failed: {err}");
                    }
                });
            }
            self.handle(parse(&event));
        }
    }

    fn handle(self: &Rc<Self>, push: Push) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        match push {
            Push::Clear { tag } => app.withdraw_notification(&format!("{ID_PREFIX}{tag}")),
            Push::RequestLocation => crate::ui::app().agent().request_location_update(),
            Push::Unsupported(message) => log::info!("Ignoring push message {message}"),
            Push::Command(command) => {
                let app = crate::ui::app();
                if !crate::control::allowed(app.settings(), &command) {
                    log::info!("Device command turned off in Preferences: {command:?}");
                    return;
                }
                glib::spawn_future_local(async move { crate::control::run(&app, command).await });
            }
            Push::Show(notice) => {
                self.counter.set(self.counter.get() + 1);
                let id = notice.id(self.counter.get());
                let notifier = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let Some(this) = notifier.upgrade() else {
                        return;
                    };
                    let notification = this.build(&notice).await;
                    if let Some(app) = this.app.upgrade() {
                        app.send_notification(Some(&id), &notification);
                    }
                });
            }
        }
    }

    async fn build(&self, notice: &Notice) -> gio::Notification {
        let notification = gio::Notification::new(&notice.title);
        if !notice.message.is_empty() {
            notification.set_body(Some(&notice.message));
        }
        notification.set_priority(notice.priority);
        if notice.click != Click::App {
            notification.set_default_action_and_target_value(
                "app.open-click",
                Some(&notice.click.target().to_variant()),
            );
        }
        let tag = notice.tag.clone().unwrap_or_default();
        for button in &notice.buttons {
            match &button.uri {
                Some(uri) => notification.add_button_with_target_value(
                    &button.title,
                    "app.open-click",
                    Some(&Click::parse(Some(uri)).target().to_variant()),
                ),
                None => notification.add_button_with_target_value(
                    &button.title,
                    "app.notification-action",
                    Some(
                        &(
                            button.action.as_str(),
                            tag.as_str(),
                            notice.title.as_str(),
                            notice.message.as_str(),
                        )
                            .to_variant(),
                    ),
                ),
            }
        }
        if let Some(image) = &notice.image
            && let Some(bytes) = self.fetch_image(image).await
        {
            notification.set_icon(&gio::BytesIcon::new(&bytes));
        }
        notification
    }

    /// Relative image paths such as `/api/camera_proxy/...` need the token.
    async fn fetch_image(&self, image: &str) -> Option<glib::Bytes> {
        let url = if image.starts_with('/') {
            format!("{}{image}", self.client.active_url().trim_end_matches('/'))
        } else {
            image.to_owned()
        };
        let token = if image.starts_with('/') {
            self.client.fresh_access_token().await.ok()
        } else {
            None
        };
        match ha::http_get(&url, token.as_deref()).await {
            Ok(response) if response.is_success() && response.body.len() <= MAX_IMAGE_BYTES => {
                Some(response.body)
            }
            Ok(response) => {
                log::info!("Notification image skipped: HTTP {}", response.status);
                None
            }
            Err(err) => {
                log::info!("Notification image failed: {err}");
                None
            }
        }
    }

    /// Fires the action event. A button pressed after Sanctum quit starts it
    /// through D-Bus with no window, so the app is held until the event is
    /// sent or the connection does not come up.
    pub fn send_action(&self, event: Value) {
        let hold = self.app.upgrade().map(|app| app.hold());
        let client = self.client.clone();
        glib::spawn_future_local(async move {
            for _ in 0..CONNECT_WAIT_STEPS {
                if client.state() == ConnectionState::Connected {
                    break;
                }
                glib::timeout_future(CONNECT_WAIT_STEP).await;
            }
            if let Err(err) = webhook::send(&client, "fire_event", event).await {
                log::warn!("Could not send notification action: {err}");
            }
            drop(hold);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notification() {
        let event = json!({
            "message": "Front door opened",
            "title": "Security",
            "data": {
                "tag": "door",
                "priority": "high",
                "clickAction": "entityId:binary_sensor.front_door",
                "actions": [
                    { "action": "SILENCE", "title": "Silence" },
                    { "action": "URI", "title": "Camera", "uri": "/lovelace/cameras" },
                    { "action": "REPLY", "title": "Reply" },
                    { "action": "A" }, { "action": "B" }
                ]
            },
            "hass_confirm_id": "c1"
        });
        let Push::Show(notice) = parse(&event) else {
            panic!("expected a notification");
        };
        assert_eq!(notice.title, "Security");
        assert_eq!(notice.id(7), "ha-door");
        assert_eq!(notice.priority, gio::NotificationPriority::High);
        assert_eq!(
            notice.click,
            Click::Entity("binary_sensor.front_door".into())
        );
        let actions: Vec<_> = notice.buttons.iter().map(|b| b.action.as_str()).collect();
        assert_eq!(actions, ["SILENCE", "URI", "A"]);
        assert_eq!(notice.buttons[1].uri.as_deref(), Some("/lovelace/cameras"));
        assert_eq!(notice.buttons[2].title, "A");
    }

    #[test]
    fn special_messages() {
        let clear = json!({ "message": "clear_notification", "data": { "tag": "door" } });
        assert_eq!(parse(&clear), Push::Clear { tag: "door".into() });
        let locate = json!({ "message": "request_location_update" });
        assert_eq!(parse(&locate), Push::RequestLocation);
        let plain = json!({ "message": "Hi" });
        let Push::Show(notice) = parse(&plain) else {
            panic!("expected a notification");
        };
        assert_eq!(notice.title, "Home Assistant");
        assert_eq!(notice.click, Click::App);
        assert_eq!(notice.id(3), "ha-#3");
    }

    #[test]
    fn click_targets_round_trip() {
        for click in [
            Click::Entity("light.kitchen".into()),
            Click::Url("/lovelace/0".into()),
            Click::Nothing,
        ] {
            assert_eq!(Click::parse(Some(&click.target())), click);
        }
    }

    #[test]
    fn action_event_data() {
        let event = action_event("SILENCE", "door", "Security", "Front door opened");
        assert_eq!(event["event_type"], "mobile_app_notification_action");
        assert_eq!(event["event_data"]["action"], "SILENCE");
        assert_eq!(event["event_data"]["tag"], "door");
        assert!(
            action_event("A", "", "", "")["event_data"]
                .get("tag")
                .is_none()
        );
    }
}
