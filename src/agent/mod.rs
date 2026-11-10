//! The device sensor agent: reads battery, network, mobile, session and system
//! state, registers each value as a `mobile_app` sensor and reports changes
//! through the webhook. Also runs the opt-in location tracker.

mod battery;
pub mod dbus;
pub mod location;
mod mobile;
mod network;
pub mod sensor;
pub mod system;

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use gtk::glib::subclass::Signal;
use gtk::glib::subclass::prelude::*;
use serde_json::Value;

use crate::control;
use crate::control::sandbox::{Permission, Policy};
use crate::ha::{self, ConnectionState, webhook};
use location::{Accuracy, Fix, Status, Tracker};
use sensor::{DEFINITIONS, Reading};

/// Changes within this window go out in one request, so a burst of D-Bus
/// signals does not become a burst of webhook calls.
const COALESCE: Duration = Duration::from_secs(3);
/// Services emit several signals per change.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(500);
const AUDIO_RESTART: u32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Source {
    Battery,
    Network,
    Mobile,
    Session,
    Polled,
    Controls,
}

impl Source {
    const ALL: [Self; 6] = [
        Self::Battery,
        Self::Network,
        Self::Mobile,
        Self::Session,
        Self::Polled,
        Self::Controls,
    ];

    /// Sensors this source owns; one missing from a new read was removed.
    fn owns(self, id: &str) -> bool {
        match self {
            Self::Battery => matches!(id, "battery_level" | "battery_state" | "charger_type"),
            Self::Network => {
                id == "network_type" || id.starts_with("wifi_") || id.starts_with("ipv")
            }
            Self::Mobile => id.starts_with("mobile_"),
            Self::Session => matches!(id, "screen_on" | "session_locked"),
            Self::Polled => matches!(id, "cpu_usage" | "memory_usage" | "last_boot"),
            Self::Controls => matches!(
                id,
                "volume_level"
                    | "volume_muted"
                    | "screen_brightness"
                    | "flashlight"
                    | "ringer_mode"
                    | "bluetooth_state"
                    | "bluetooth_connection"
                    | "media_session"
                    | "do_not_disturb"
            ),
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Agent {
        pub(super) client: OnceCell<ha::Client>,
        pub(super) settings: OnceCell<gio::Settings>,
        pub(super) server: OnceCell<std::rc::Rc<crate::server_settings::ServerSettings>>,
        pub(super) started: Cell<bool>,
        /// Set while Sanctum runs as a front end or is signed out: nothing is read
        /// or sent.
        pub(super) paused: Cell<bool>,
        pub(super) system: RefCell<Option<gio::DBusConnection>>,
        pub(super) session: RefCell<Option<gio::DBusConnection>>,
        pub(super) upower: Cell<bool>,
        pub(super) screen_on: Cell<Option<bool>>,
        pub(super) readings: RefCell<HashMap<&'static str, Reading>>,
        /// Last value Home Assistant accepted per sensor, for change detection.
        pub(super) sent: RefCell<HashMap<&'static str, Reading>>,
        /// Registered on the current connection; cleared on every reconnect so
        /// the latest values are re-sent.
        pub(super) registered: RefCell<HashSet<&'static str>>,
        pub(super) dirty: RefCell<HashSet<&'static str>>,
        pub(super) flush_source: RefCell<Option<glib::SourceId>>,
        pub(super) flushing: Cell<bool>,
        pub(super) refresh_sources: RefCell<HashMap<Source, glib::SourceId>>,
        pub(super) poll_source: RefCell<Option<glib::SourceId>>,
        pub(super) audio_watch: RefCell<Option<gio::Subprocess>>,
        pub(super) settings_monitor: RefCell<Option<gio::FileMonitor>>,
        pub(super) subscriptions: RefCell<Vec<gio::SignalSubscription>>,
        pub(super) cpu: system::CpuSampler,
        pub(super) tracker: OnceCell<Rc<Tracker>>,
        pub(super) last_fix: RefCell<Option<Fix>>,
        pub(super) fix_unsent: Cell<bool>,
        /// Unix seconds of the last location Home Assistant accepted.
        pub(super) location_sent_at: Cell<i64>,
        /// Set once a lost registration was renewed this run, so a server that
        /// keeps rejecting the webhook does not cause a loop.
        pub(super) renewed: Cell<bool>,
        /// The disabled set as last applied, to find which sensors toggled.
        pub(super) disabled: RefCell<HashSet<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Agent {
        const NAME: &'static str = "SanctumAgent";
        type Type = super::Agent;
    }

    impl ObjectImpl for Agent {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            // Readings or location status changed; settings pages refresh on it.
            SIGNALS.get_or_init(|| vec![Signal::builder("changed").build()])
        }
    }
}

glib::wrapper! {
    pub struct Agent(ObjectSubclass<imp::Agent>);
}

impl Agent {
    pub fn new(
        client: &ha::Client,
        settings: &gio::Settings,
        server: &std::rc::Rc<crate::server_settings::ServerSettings>,
    ) -> Self {
        let agent: Self = glib::Object::new();
        let _ = agent.imp().client.set(client.clone());
        let _ = agent.imp().settings.set(settings.clone());
        let _ = agent.imp().server.set(server.clone());
        agent
    }

    /// The signed-in server's settings, which hold which sensors are off and
    /// whether location is reported.
    fn server(&self) -> gio::Settings {
        self.imp().server.get().expect("set in new()").get()
    }

    fn client(&self) -> &ha::Client {
        self.imp().client.get().expect("set in new()")
    }

    fn settings(&self) -> &gio::Settings {
        self.imp().settings.get().expect("set in new()")
    }

    pub fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("changed", false, move |values| {
            let agent = values[0].get::<Self>().expect("signal emitter is an Agent");
            f(&agent);
            None
        })
    }

    fn emit_changed(&self) {
        self.emit_by_name::<()>("changed", &[]);
    }

    /// Current readings in definition order.
    pub fn readings(&self) -> Vec<Reading> {
        let readings = self.imp().readings.borrow();
        DEFINITIONS
            .iter()
            .filter_map(|definition| readings.get(definition.id).cloned())
            .collect()
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        !self
            .server()
            .strv("disabled-sensors")
            .iter()
            .any(|disabled| disabled == id)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) {
        let mut disabled: Vec<String> = self
            .server()
            .strv("disabled-sensors")
            .iter()
            .map(|value| value.to_string())
            .filter(|value| value != id)
            .collect();
        if !enabled {
            disabled.push(id.to_owned());
        }
        let _ = self.server().set_strv("disabled-sensors", disabled);
    }

    pub fn location_status(&self) -> Status {
        self.imp()
            .tracker
            .get()
            .map(|tracker| tracker.status())
            .unwrap_or_default()
    }

    /// Unix seconds, or 0 before the first report.
    pub fn location_sent_at(&self) -> i64 {
        self.imp().location_sent_at.get()
    }

    /// Starts reading and reporting. Calling it again resumes after `pause`.
    pub fn start(&self) {
        let imp = self.imp();
        if imp.started.replace(true) {
            if imp.paused.replace(false) {
                self.schedule_poll();
                self.on_connection(self.client().state());
                self.watch_audio();
            }
            self.apply_location();
            return;
        }
        let weak = self.downgrade();
        let tracker = Tracker::new(
            {
                let weak = weak.clone();
                move |fix| {
                    if let Some(agent) = weak.upgrade() {
                        agent.on_fix(fix);
                    }
                }
            },
            move || {
                if let Some(agent) = weak.upgrade() {
                    agent.emit_changed();
                }
            },
        );
        let _ = imp.tracker.set(tracker);
        imp.disabled.replace(self.disabled_set());

        self.client().connect_state_notify(glib::clone!(
            #[weak(rename_to = agent)]
            self,
            move |client| agent.on_connection(client.state())
        ));
        let settings = self.settings();
        let weak = self.downgrade();
        let server = self.imp().server.get().expect("set in new()");
        server.watch(&["disabled-sensors"], {
            let weak = weak.clone();
            move |_| {
                if let Some(agent) = weak.upgrade() {
                    agent.on_disabled_changed();
                }
            }
        });
        server.watch(&["location-enabled"], move |_| {
            if let Some(agent) = weak.upgrade() {
                agent.apply_location();
            }
        });
        settings.connect_changed(
            Some("sensor-interval"),
            glib::clone!(
                #[weak(rename_to = agent)]
                self,
                move |_, _| agent.schedule_poll()
            ),
        );
        for key in ["location-accuracy", "location-interval"] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak(rename_to = agent)]
                    self,
                    move |_, _| agent.apply_location()
                ),
            );
        }

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = agent)]
            self,
            async move { agent.connect_sources().await }
        ));
        self.apply_location();
    }

    async fn connect_sources(&self) {
        let imp = self.imp();
        let system = dbus::system().await;
        let session = dbus::session().await;
        imp.system.replace(system.clone());
        imp.session.replace(session.clone());
        let mut subscriptions = Vec::new();
        let refresh = |source: Source| {
            let weak = self.downgrade();
            move || {
                if let Some(agent) = weak.upgrade() {
                    agent.refresh(source);
                }
            }
        };
        if let Some(system) = &system {
            imp.upower
                .set(dbus::has_owner(system, battery::UPOWER).await);
            subscriptions.push(dbus::watch(
                system,
                battery::UPOWER,
                None,
                None,
                None,
                refresh(Source::Battery),
            ));
            subscriptions.push(dbus::watch(
                system,
                network::NETWORK_MANAGER,
                None,
                None,
                None,
                refresh(Source::Network),
            ));
            subscriptions.push(dbus::watch(
                system,
                mobile::MODEM_MANAGER,
                None,
                None,
                None,
                refresh(Source::Mobile),
            ));
            subscriptions.push(dbus::watch(
                system,
                system::LOGIND,
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                Some(system::LOGIND_SESSION),
                refresh(Source::Session),
            ));
            subscriptions.push(dbus::watch(
                system,
                control::bluetooth::BLUEZ,
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                None,
                refresh(Source::Controls),
            ));
        }
        if let Some(session) = &session {
            subscriptions.push(session.subscribe_to_signal(
                None,
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                Some(control::media::PATH),
                Some(control::media::PLAYER),
                gio::DBusSignalFlags::NONE,
                {
                    let refresh = refresh(Source::Controls);
                    move |_| refresh()
                },
            ));
            subscriptions.push(session.subscribe_to_signal(
                Some("org.freedesktop.DBus"),
                Some("org.freedesktop.DBus"),
                Some("NameOwnerChanged"),
                None,
                Some(control::media::NAMESPACE),
                gio::DBusSignalFlags::MATCH_ARG0_NAMESPACE,
                {
                    let refresh = refresh(Source::Controls);
                    move |_| refresh()
                },
            ));
            subscriptions.push(dbus::watch(
                session,
                control::display::FLASHLIGHTD,
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                None,
                refresh(Source::Controls),
            ));
            subscriptions.push(dbus::watch(
                session,
                control::ringer::FEEDBACKD,
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                None,
                refresh(Source::Controls),
            ));
            subscriptions.push(dbus::watch(
                session,
                system::DISPLAY_CONFIG,
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                None,
                refresh(Source::Session),
            ));
        }
        imp.subscriptions.replace(subscriptions);
        self.watch_audio();
        // dconf replaces its database file on every write.
        if let Ok(monitor) = gio::File::for_path(control::settings::database_path())
            .monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            let refresh = refresh(Source::Controls);
            monitor.connect_changed(move |_, _, _, _| refresh());
            imp.settings_monitor.replace(Some(monitor));
        }
        for source in Source::ALL {
            self.refresh(source);
        }
        self.schedule_poll();
    }

    pub fn refresh_all(&self) {
        if self.imp().started.get() {
            for source in Source::ALL {
                self.refresh(source);
            }
        }
    }

    pub fn refresh_controls(&self) {
        if self.imp().started.get() {
            self.refresh(Source::Controls);
        }
    }

    /// `pactl subscribe` is restarted a while after the sound server goes
    /// away, unless the agent was paused in the meantime.
    fn watch_audio(&self) {
        if self.imp().audio_watch.borrow().is_some() {
            return;
        }
        let weak = self.downgrade();
        let Some(process) = control::audio::watch(move || {
            if let Some(agent) = weak.upgrade() {
                agent.refresh(Source::Controls);
            }
        }) else {
            return;
        };
        let weak = self.downgrade();
        let exited = process.wait_future();
        let watched = process.clone();
        glib::spawn_future_local(async move {
            let _ = exited.await;
            let Some(agent) = weak.upgrade() else { return };
            let imp = agent.imp();
            if imp.audio_watch.borrow().as_ref() == Some(&watched) {
                imp.audio_watch.replace(None);
            }
            let weak = agent.downgrade();
            glib::timeout_add_seconds_local_once(AUDIO_RESTART, move || {
                if let Some(agent) = weak.upgrade()
                    && !agent.imp().paused.get()
                {
                    agent.refresh(Source::Controls);
                    agent.watch_audio();
                }
            });
        });
        self.imp().audio_watch.replace(Some(process));
    }

    fn stop_audio_watch(&self) {
        if let Some(process) = self.imp().audio_watch.take() {
            process.force_exit();
        }
    }

    /// Re-reads one source shortly, folding bursts of signals into one read.
    fn refresh(&self, source: Source) {
        let imp = self.imp();
        if let Some(pending) = imp.refresh_sources.borrow_mut().remove(&source) {
            pending.remove();
        }
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(REFRESH_DEBOUNCE, move || {
            let Some(agent) = weak.upgrade() else { return };
            agent.imp().refresh_sources.borrow_mut().remove(&source);
            glib::spawn_future_local(async move {
                let readings = agent.read(source).await;
                agent.apply(source, readings);
            });
        });
        imp.refresh_sources.borrow_mut().insert(source, id);
    }

    async fn read(&self, source: Source) -> Vec<Reading> {
        let imp = self.imp();
        let system = imp.system.borrow().clone();
        let session = imp.session.borrow().clone();
        match source {
            Source::Battery => {
                let upower = system.as_ref().filter(|_| imp.upower.get());
                battery::read(upower).await
            }
            Source::Network => match &system {
                Some(system) => network::read(system).await,
                None => Vec::new(),
            },
            Source::Mobile => match &system {
                Some(system) => mobile::read(system).await,
                None => Vec::new(),
            },
            Source::Session => {
                let readings = system::read_session(system.as_ref(), session.as_ref()).await;
                imp.screen_on.set(
                    readings
                        .iter()
                        .find(|reading| reading.id == "screen_on")
                        .and_then(|reading| reading.state.as_bool()),
                );
                readings
            }
            Source::Polled => system::read_polled(&imp.cpu),
            Source::Controls => read_controls().await,
        }
    }

    fn apply(&self, source: Source, readings: Vec<Reading>) {
        let imp = self.imp();
        let network_before = self.network_identity();
        {
            let mut current = imp.readings.borrow_mut();
            current.retain(|id, _| !source.owns(id));
            let sent = imp.sent.borrow();
            let mut dirty = imp.dirty.borrow_mut();
            for reading in readings {
                if sent.get(reading.id) != Some(&reading) {
                    dirty.insert(reading.id);
                }
                current.insert(reading.id, reading);
            }
        }
        if source == Source::Network && network_before.is_some() {
            let network_after = self.network_identity();
            if network_after != network_before {
                // A new network can mean a new place, such as leaving home
                // Wi-Fi, so a fresh fix is requested.
                self.request_location_update();
            }
        }
        self.emit_changed();
        self.schedule_flush();
    }

    fn network_identity(&self) -> Option<(Value, Value)> {
        let readings = self.imp().readings.borrow();
        Some((
            readings.get("network_type")?.state.clone(),
            readings.get("wifi_bssid")?.state.clone(),
        ))
    }

    fn schedule_poll(&self) {
        let imp = self.imp();
        if let Some(source) = imp.poll_source.take() {
            source.remove();
        }
        if imp.paused.get() {
            return;
        }
        let interval = self.settings().uint("sensor-interval").max(15);
        let id = glib::timeout_add_seconds_local(
            interval,
            glib::clone!(
                #[weak(rename_to = agent)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    // Polling pauses while the screen is known to be off.
                    if agent.imp().screen_on.get() != Some(false) {
                        agent.refresh(Source::Polled);
                        // Brightness and a sysfs torch emit no change signals.
                        agent.refresh(Source::Controls);
                        if !agent.imp().upower.get() {
                            agent.refresh(Source::Battery);
                        }
                    }
                    glib::ControlFlow::Continue
                }
            ),
        );
        imp.poll_source.replace(Some(id));
    }

    fn disabled_set(&self) -> HashSet<String> {
        self.server()
            .strv("disabled-sensors")
            .iter()
            .map(|id| id.to_string())
            .collect()
    }

    fn on_disabled_changed(&self) {
        // Toggled sensors are registered again so Home Assistant enables or
        // disables the entity to match.
        let imp = self.imp();
        let disabled = self.disabled_set();
        let previous = imp.disabled.replace(disabled.clone());
        let ids: Vec<&'static str> = imp.readings.borrow().keys().copied().collect();
        for id in ids {
            if disabled.contains(id) != previous.contains(id) {
                imp.registered.borrow_mut().remove(id);
                imp.dirty.borrow_mut().insert(id);
            }
        }
        self.emit_changed();
        self.schedule_flush();
    }

    fn on_connection(&self, state: ConnectionState) {
        if state != ConnectionState::Connected || self.imp().paused.get() {
            return;
        }
        let imp = self.imp();
        imp.registered.borrow_mut().clear();
        let ids: Vec<&'static str> = imp.readings.borrow().keys().copied().collect();
        imp.dirty.borrow_mut().extend(ids);
        self.schedule_flush();
        if imp.fix_unsent.get() {
            self.send_location();
        }
    }

    fn schedule_flush(&self) {
        let imp = self.imp();
        if imp.paused.get() || imp.flush_source.borrow().is_some() || imp.dirty.borrow().is_empty()
        {
            return;
        }
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(COALESCE, move || {
            let Some(agent) = weak.upgrade() else { return };
            agent.imp().flush_source.take();
            glib::spawn_future_local(async move { agent.flush().await });
        });
        imp.flush_source.replace(Some(id));
    }

    async fn flush(&self) {
        let imp = self.imp();
        let client = self.client().clone();
        if client.state() != ConnectionState::Connected || imp.flushing.replace(true) {
            return;
        }
        let mut dirty: Vec<&'static str> = imp.dirty.borrow_mut().drain().collect();
        let mut updates = Vec::new();
        let mut failed = false;
        while let Some(id) = dirty.pop() {
            let Some(reading) = imp.readings.borrow().get(id).cloned() else {
                continue;
            };
            let enabled = self.is_enabled(id);
            if !imp.registered.borrow().contains(id) {
                match webhook::send(&client, "register_sensor", reading.registration(!enabled))
                    .await
                {
                    Ok(_) => {
                        imp.registered.borrow_mut().insert(id);
                        imp.sent.borrow_mut().insert(id, reading);
                    }
                    Err(err) => {
                        // The rest waits for the next flush.
                        let mut pending = imp.dirty.borrow_mut();
                        pending.insert(id);
                        pending.extend(dirty.drain(..));
                        pending.extend(updates.drain(..).map(|reading: Reading| reading.id));
                        drop(pending);
                        self.on_webhook_error(err);
                        failed = true;
                        break;
                    }
                }
            } else if enabled {
                updates.push(reading);
            }
        }
        if !updates.is_empty() {
            let data = Value::Array(updates.iter().map(Reading::update).collect());
            match webhook::send(&client, "update_sensor_states", data).await {
                Ok(reply) => {
                    let unregistered = webhook::unregistered(&reply);
                    for reading in updates {
                        if unregistered.iter().any(|id| id == reading.id) {
                            imp.registered.borrow_mut().remove(reading.id);
                            imp.dirty.borrow_mut().insert(reading.id);
                        } else {
                            imp.sent.borrow_mut().insert(reading.id, reading);
                        }
                    }
                }
                Err(err) => {
                    imp.dirty
                        .borrow_mut()
                        .extend(updates.iter().map(|reading| reading.id));
                    self.on_webhook_error(err);
                    failed = true;
                }
            }
        }
        imp.flushing.set(false);
        // Failures are retried by the next reading or reconnect; rescheduling
        // here would hammer a server that keeps failing.
        if !failed {
            self.schedule_flush();
        }
    }

    fn on_webhook_error(&self, err: ha::Error) {
        if err == ha::Error::RegistrationGone && !self.imp().renewed.replace(true) {
            log::warn!("Device registration was deleted in Home Assistant; registering again");
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = agent)]
                self,
                async move { agent.renew_registration().await }
            ));
        } else {
            log::info!("Sensor update failed: {err}");
        }
    }

    async fn renew_registration(&self) {
        let client = self.client().clone();
        let Some(credentials) = client.credentials() else {
            return;
        };
        let base = ha::ServerUrl::parse(&client.active_url()).unwrap_or(credentials.server);
        let result = async {
            let token = client.fresh_access_token().await?;
            let device = ha::device::DeviceInfo::detect(&self.server().string("device-name"));
            ha::registration::register(&base, &token, &credentials.device_id, &device).await
        }
        .await;
        match result {
            Ok(registration) => {
                client.replace_registration(registration).await;
                let imp = self.imp();
                imp.registered.borrow_mut().clear();
                let ids: Vec<&'static str> = imp.readings.borrow().keys().copied().collect();
                imp.dirty.borrow_mut().extend(ids);
                self.schedule_flush();
            }
            Err(err) => log::warn!("Could not register the device again: {err}"),
        }
    }

    fn apply_location(&self) {
        if self.imp().paused.get() {
            return;
        }
        let Some(tracker) = self.imp().tracker.get() else {
            return;
        };
        let settings = self.settings();
        if self.server().boolean("location-enabled") {
            tracker.start(
                Accuracy::from_setting(&settings.string("location-accuracy")),
                settings.uint("location-interval"),
            );
        } else {
            tracker.stop();
        }
    }

    /// For front-end use and sign-out: readings, location and sending stop
    /// until `start`.
    pub fn pause(&self) {
        let imp = self.imp();
        imp.paused.set(true);
        if let Some(source) = imp.poll_source.take() {
            source.remove();
        }
        if let Some(source) = imp.flush_source.take() {
            source.remove();
        }
        self.stop_audio_watch();
        self.stop_location();
    }

    fn stop_location(&self) {
        if let Some(tracker) = self.imp().tracker.get() {
            tracker.stop();
        }
        self.imp().last_fix.replace(None);
        self.imp().fix_unsent.set(false);
    }

    /// Asks for a fresh fix, as for HA's `request_location_update` command.
    pub fn request_location_update(&self) {
        if self.server().boolean("location-enabled") {
            self.apply_location();
        }
    }

    fn on_fix(&self, fix: Fix) {
        let imp = self.imp();
        imp.last_fix.replace(Some(fix));
        imp.fix_unsent.set(true);
        self.send_location();
    }

    fn send_location(&self) {
        let imp = self.imp();
        if imp.paused.get() {
            return;
        }
        let Some(fix) = imp.last_fix.borrow().clone() else {
            return;
        };
        let client = self.client().clone();
        if client.state() != ConnectionState::Connected {
            return;
        }
        let battery = imp
            .readings
            .borrow()
            .get("battery_level")
            .and_then(|reading| reading.state.as_i64());
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = agent)]
            self,
            async move {
                match webhook::send(&client, "update_location", fix.payload(battery)).await {
                    Ok(_) => {
                        let imp = agent.imp();
                        imp.fix_unsent.set(false);
                        imp.location_sent_at.set(glib::real_time() / 1_000_000);
                        agent.emit_changed();
                    }
                    Err(err) => agent.on_webhook_error(err),
                }
            }
        ));
    }
}

impl Default for Agent {
    fn default() -> Self {
        glib::Object::new()
    }
}

async fn read_controls() -> Vec<Reading> {
    let mut readings = Vec::new();
    if let Some(volume) = control::audio::read().await {
        readings.push(Reading::new("volume_level", volume.level));
        readings.push(Reading::new("volume_muted", volume.muted));
    }
    if let Some(backlight) = control::display::backlight() {
        readings.push(Reading::new("screen_brightness", backlight.percent()));
    }
    if let Some(flashlight) = control::display::flashlight().await {
        readings.push(Reading::new("flashlight", flashlight.is_on()));
    }
    if let Some(mode) = control::ringer::read().await {
        readings.push(Reading::new("ringer_mode", mode.name()));
    }
    if let Some(bluetooth) = control::bluetooth::state().await {
        readings.push(Reading::new("bluetooth_state", bluetooth.powered));
        readings.push(
            Reading::new("bluetooth_connection", bluetooth.connected.len())
                .attribute("connected_devices", bluetooth.connected),
        );
    }
    if Policy::current().allows(Permission::Filesystem(control::settings::DATABASE))
        && let Some(on) = control::settings::do_not_disturb()
    {
        readings.push(Reading::new("do_not_disturb", on));
    }
    // Without the permission no players are visible, which would read as none.
    let mpris = Permission::SessionTalk(control::media::MPRIS);
    if Policy::current().allows(mpris)
        && let Some(media) = control::media::session().await
    {
        let mut reading = Reading::new("media_session", media.state);
        for (key, value) in [
            ("player", media.player),
            ("title", media.title),
            ("artist", media.artist),
            ("album", media.album),
        ] {
            if let Some(value) = value {
                reading = reading.attribute(key, value);
            }
        }
        readings.push(reading);
    }
    readings
}
