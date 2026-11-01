//! Location from GeoClue through the location portal, sent to Home Assistant's
//! device tracker with `update_location`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::glib::prelude::*;
use serde_json::{Value, json};

use super::dbus::{self, PropertyMap};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const LOCATION: &str = "org.freedesktop.portal.Location";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accuracy {
    City,
    Street,
    Exact,
}

impl Accuracy {
    pub fn from_setting(value: &str) -> Self {
        match value {
            "city" => Self::City,
            "exact" => Self::Exact,
            _ => Self::Street,
        }
    }

    /// The portal's levels: 2 city, 4 street, 5 exact.
    fn portal_level(self) -> u32 {
        match self {
            Self::City => 2,
            Self::Street => 4,
            Self::Exact => 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fix {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy: f64,
    pub altitude: Option<f64>,
    pub speed: Option<f64>,
    pub heading: Option<f64>,
}

impl Fix {
    /// Unknown values arrive as negative numbers, or -G_MAXDOUBLE for altitude.
    fn from_portal(location: &HashMap<String, glib::Variant>) -> Option<Self> {
        let known = |key: &str, min: f64| location.value::<f64>(key).filter(|v| *v >= min);
        Some(Self {
            latitude: location.value("Latitude")?,
            longitude: location.value("Longitude")?,
            accuracy: location.value::<f64>("Accuracy").unwrap_or(0.0).max(0.0),
            altitude: known("Altitude", -1.0e6),
            speed: known("Speed", 0.0),
            heading: known("Heading", 0.0),
        })
    }

    /// `update_location` data.
    pub fn payload(&self, battery: Option<i64>) -> Value {
        let mut data = json!({
            "gps": [self.latitude, self.longitude],
            "gps_accuracy": self.accuracy.round() as i64,
        });
        for (key, value) in [
            ("altitude", self.altitude),
            ("speed", self.speed),
            ("course", self.heading),
        ] {
            if let Some(value) = value {
                data[key] = json!(value.round() as i64);
            }
        }
        if let Some(battery) = battery {
            data["battery"] = json!(battery);
        }
        data
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Off,
    Starting,
    Running,
    /// The portal refused or is missing; the text is shown in settings.
    Failed(String),
}

type FixHandler = Rc<dyn Fn(Fix)>;

/// One portal session at a time; settings changes and network changes start a
/// new one.
pub struct Tracker {
    session: RefCell<Option<String>>,
    subscription: RefCell<Option<gio::SignalSubscription>>,
    status: RefCell<Status>,
    generation: Cell<u64>,
    on_fix: FixHandler,
    on_status: Rc<dyn Fn()>,
}

impl Tracker {
    pub fn new(on_fix: impl Fn(Fix) + 'static, on_status: impl Fn() + 'static) -> Rc<Self> {
        Rc::new(Self {
            session: RefCell::default(),
            subscription: RefCell::default(),
            status: RefCell::default(),
            generation: Cell::new(0),
            on_fix: Rc::new(on_fix),
            on_status: Rc::new(on_status),
        })
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    fn set_status(&self, status: Status) {
        if *self.status.borrow() != status {
            self.status.replace(status);
            (self.on_status)();
        }
    }

    pub fn stop(&self) {
        self.generation.set(self.generation.get() + 1);
        self.subscription.replace(None);
        if let Some(session) = self.session.take() {
            glib::spawn_future_local(async move {
                if let Some(bus) = dbus::session().await {
                    let _ = dbus::call(
                        &bus,
                        PORTAL,
                        &session,
                        "org.freedesktop.portal.Session",
                        "Close",
                        None,
                    )
                    .await;
                }
            });
        }
        self.set_status(Status::Off);
    }

    /// Starts a new session; the portal sends a fresh fix soon after.
    pub fn start(self: &Rc<Self>, accuracy: Accuracy, interval_seconds: u32) {
        self.stop();
        let generation = self.generation.get();
        self.set_status(Status::Starting);
        let tracker = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = start_session(&tracker, generation, accuracy, interval_seconds).await;
            if let Some(tracker) = tracker.upgrade()
                && tracker.generation.get() == generation
            {
                match result {
                    Ok(()) => tracker.set_status(Status::Running),
                    Err(message) => {
                        log::info!("Location unavailable: {message}");
                        tracker.subscription.replace(None);
                        tracker.set_status(Status::Failed(message));
                    }
                }
            }
        });
    }
}

fn token() -> String {
    format!("sanctum{}", glib::random_int())
}

async fn start_session(
    tracker: &std::rc::Weak<Tracker>,
    generation: u64,
    accuracy: Accuracy,
    interval_seconds: u32,
) -> Result<(), String> {
    let bus = dbus::session()
        .await
        .ok_or_else(|| "no session bus".to_owned())?;
    let options = glib::VariantDict::new(None);
    options.insert("session_handle_token", token());
    options.insert("distance-threshold", 0u32);
    options.insert("time-threshold", interval_seconds);
    options.insert("accuracy", accuracy.portal_level());
    let reply = dbus::call(
        &bus,
        PORTAL,
        PORTAL_PATH,
        LOCATION,
        "CreateSession",
        // A Variant inside a Rust tuple would be boxed as `v`, so the
        // `(a{sv})` arguments are built from parts.
        Some(glib::Variant::tuple_from_iter([options.end()])),
    )
    .await
    .map_err(|err| err.message().to_owned())?;
    let session = reply
        .child_value(0)
        .str()
        .ok_or_else(|| "unexpected portal reply".to_owned())?
        .to_owned();

    let Some(owner) = tracker.upgrade() else {
        return Ok(());
    };
    if owner.generation.get() != generation {
        return Ok(());
    }
    owner.session.replace(Some(session.clone()));
    let on_fix = owner.on_fix.clone();
    let session_path = session.clone();
    let subscription = bus.subscribe_to_signal(
        Some(PORTAL),
        Some(LOCATION),
        Some("LocationUpdated"),
        Some(PORTAL_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            let parameters = signal.parameters;
            if parameters.child_value(0).str() != Some(session_path.as_str()) {
                return;
            }
            let location = parameters
                .child_value(1)
                .get::<HashMap<String, glib::Variant>>();
            if let Some(fix) = location.as_ref().and_then(Fix::from_portal) {
                on_fix(fix);
            }
        },
    );
    owner.subscription.replace(Some(subscription));
    drop(owner);

    // The Response signal can arrive before Start returns, so the request
    // path is predicted and watched first.
    let handle_token = token();
    let sender = bus
        .unique_name()
        .map(|name| name.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let request = format!("{PORTAL_PATH}/request/{sender}/{handle_token}");
    let (response_sender, response) = futures_channel::oneshot::channel::<u32>();
    let response_sender = RefCell::new(Some(response_sender));
    let _watch = bus.subscribe_to_signal(
        Some(PORTAL),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&request),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            if let (Some(code), Some(sender)) = (
                signal.parameters.child_value(0).get::<u32>(),
                response_sender.take(),
            ) {
                let _ = sender.send(code);
            }
        },
    );
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", handle_token);
    dbus::call(
        &bus,
        PORTAL,
        PORTAL_PATH,
        LOCATION,
        "Start",
        Some(glib::Variant::tuple_from_iter([
            glib::variant::ObjectPath::try_from(session)
                .map_err(|_| "unexpected portal reply".to_owned())?
                .to_variant(),
            "".to_variant(),
            options.end(),
        ])),
    )
    .await
    .map_err(|err| err.message().to_owned())?;
    match response.await {
        Ok(0) => Ok(()),
        Ok(1) => Err("Location access was denied".to_owned()),
        _ => Err("Location is unavailable".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_fix_payload() {
        let mut location = HashMap::new();
        location.insert("Latitude".to_owned(), 41.88.to_variant());
        location.insert("Longitude".to_owned(), (-87.63).to_variant());
        location.insert("Accuracy".to_owned(), 12.4.to_variant());
        location.insert("Altitude".to_owned(), f64::MIN.to_variant());
        location.insert("Speed".to_owned(), (-1.0).to_variant());
        location.insert("Heading".to_owned(), 90.0.to_variant());
        let fix = Fix::from_portal(&location).unwrap();
        let payload = fix.payload(Some(80));
        assert_eq!(payload["gps"], json!([41.88, -87.63]));
        assert_eq!(payload["gps_accuracy"], 12);
        assert_eq!(payload["course"], 90);
        assert_eq!(payload["battery"], 80);
        assert!(payload.get("altitude").is_none());
        assert!(payload.get("speed").is_none());
    }
}
