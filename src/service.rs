//! `sanctum-service`: the Home Assistant connection for sensors,
//! notifications and device commands, without GTK or WebKit. The window app
//! starts it, tells it when the sign-in changes, and reads its state through
//! the actions it exports on D-Bus.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gettextrs::gettext;
use gio::prelude::*;

use crate::agent::Agent;
use crate::config::APP_ID;
use crate::control::{self, Forward};
use crate::ha;
use crate::notify::{self, Event, Notifier};
use crate::reregister::Reregistration;
use crate::server_settings::ServerSettings;

/// Actions the service exports.
pub mod action {
    /// Reads the sign-in again after the window app changed it.
    pub const RELOAD: &str = "reload";
    pub const QUIT: &str = "quit";
    /// Boolean state, set when automatic registration attempts gave up.
    pub const REGISTRATION_ALERT: &str = "registration-alert";
    /// String state: the agent's `Snapshot` as JSON.
    pub const SENSORS: &str = "sensors";
}

/// Actions of the window app that the service activates.
pub mod window_action {
    pub const OPEN_CLICK: &str = "open-click";
    pub const NOTIFICATION_ACTION: &str = "notification-action";
    pub const REGISTER_AGAIN: &str = "register-again";
    pub const STOP_PLAYBACK: &str = "stop-playback";
    /// Boolean state: the main window is on screen.
    pub const WINDOW_SHOWN: &str = "main-window-shown";
}

/// The ID of the notification sent when automatic registration gives up.
pub const REGISTRATION_NOTIFICATION: &str = "registration";
/// Used while the keyring cannot be read.
const KEYRING_RETRY: Duration = Duration::from_secs(30);
const KEYRING_ATTEMPTS: u32 = 10;

pub fn id() -> String {
    format!("{APP_ID}.Service")
}

/// The object path GApplication exports an application ID's actions on.
pub fn object_path(id: &str) -> String {
    format!("/{}", id.replace('.', "/").replace('-', "_"))
}

pub fn run() -> glib::ExitCode {
    let app = gio::Application::new(Some(&id()), gio::ApplicationFlags::empty());
    let service: Rc<OnceCell<Rc<Service>>> = Rc::default();
    app.connect_startup(move |app| {
        let created = Service::new(app);
        created.start();
        let _ = service.set(created);
    });
    // GLib warns when `activate` has no handler; startup does the work.
    app.connect_activate(|_| {});
    app.run()
}

pub struct Service {
    app: gio::Application,
    settings: gio::Settings,
    server: Rc<ServerSettings>,
    client: ha::Client,
    agent: Agent,
    notifier: OnceCell<Rc<Notifier>>,
    reregistration: Reregistration,
    alert: gio::SimpleAction,
    sensors: gio::SimpleAction,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    /// Whether the window app is on the bus.
    window_app: Cell<bool>,
    /// Bumped by each reload and by `stop`; an older reload's result is
    /// dropped.
    generation: Cell<u64>,
    /// The last message sent to the Background portal.
    status: RefCell<String>,
    /// Set when the portal rejected a status, so no more are sent.
    status_unsupported: Rc<Cell<bool>>,
    /// Set from a lost connection until the next one, so each retry's brief
    /// Connecting state does not change the status.
    reconnecting: Cell<bool>,
}

impl Service {
    fn new(app: &gio::Application) -> Rc<Self> {
        let settings = gio::Settings::new(APP_ID);
        let server = ServerSettings::new(&settings);
        let client = ha::Client::default();
        let agent = Agent::new(&client, &settings, &server);
        let alert =
            gio::SimpleAction::new_stateful(action::REGISTRATION_ALERT, None, &false.to_variant());
        let sensors =
            gio::SimpleAction::new_stateful(action::SENSORS, None, &String::new().to_variant());
        // Requests from other processes to change these states are ignored.
        for state in [&alert, &sensors] {
            state.connect_activate(|_, _| {});
            state.connect_change_state(|_, _| {});
        }
        let this = Rc::new(Self {
            app: app.clone(),
            settings,
            server,
            client,
            agent,
            notifier: OnceCell::new(),
            reregistration: Reregistration::default(),
            alert,
            sensors,
            hold: RefCell::default(),
            window_app: Cell::new(false),
            generation: Cell::new(0),
            status: RefCell::default(),
            status_unsupported: Rc::default(),
            reconnecting: Cell::new(false),
        });
        let weak = Rc::downgrade(&this);
        let notifier = Notifier::new(app, &this.client, move |event| {
            if let Some(this) = weak.upgrade() {
                this.on_push(event);
            }
        });
        let _ = this.notifier.set(notifier);
        this.setup();
        this
    }

    fn notifier(&self) -> &Rc<Notifier> {
        self.notifier.get().expect("set in new()")
    }

    fn setup(self: &Rc<Self>) {
        ha::trust::set_trusted(
            self.settings
                .strv("trusted-certificates")
                .iter()
                .map(ToString::to_string),
        );
        self.settings
            .connect_changed(Some("trusted-certificates"), |settings, key| {
                ha::trust::set_trusted(settings.strv(key).iter().map(ToString::to_string));
            });
        let weak = Rc::downgrade(self);
        self.settings
            .connect_changed(Some("run-in-background"), move |_, _| {
                if let Some(this) = weak.upgrade() {
                    this.check_lifetime();
                }
            });
        let weak = Rc::downgrade(self);
        self.server.watch(
            &["internal-url", "external-url", "home-networks"],
            move |_| {
                if let Some(this) = weak.upgrade() {
                    let registration = this.client.credentials().and_then(|c| c.registration);
                    this.client
                        .set_endpoints(this.server.endpoints(registration.as_ref()));
                }
            },
        );

        let weak = Rc::downgrade(self);
        self.client.connect_state_notify(move |client| {
            if let Some(this) = weak.upgrade() {
                match client.state() {
                    ha::ConnectionState::Disconnected => this.reconnecting.set(true),
                    ha::ConnectionState::Connecting => {}
                    _ => this.reconnecting.set(false),
                }
                this.update_status();
            }
        });
        let weak = Rc::downgrade(self);
        self.client.connect_location_name_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.update_status();
            }
        });

        let sensors = self.sensors.clone();
        self.agent.connect_changed(move |agent| {
            sensors.set_state(&agent.snapshot().to_json().to_variant());
        });
        let weak = Rc::downgrade(self);
        self.agent.connect_registration_lost(move |_| {
            if let Some(this) = weak.upgrade() {
                this.registration_lost();
            }
        });

        self.setup_actions();
    }

    fn setup_actions(self: &Rc<Self>) {
        let app = &self.app;
        app.add_action(&self.alert);
        app.add_action(&self.sensors);

        let reload = gio::SimpleAction::new(action::RELOAD, None);
        let weak = Rc::downgrade(self);
        reload.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });
        app.add_action(&reload);

        let quit = gio::SimpleAction::new(action::QUIT, None);
        let weak = Rc::downgrade(self);
        quit.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.stop();
            }
        });
        app.add_action(&quit);

        // Notifications name these actions. Outside Flatpak, GLib delivers
        // them to the process that sent the notification, which is this one.
        let open = gio::SimpleAction::new(window_action::OPEN_CLICK, Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        open.connect_activate(move |_, parameter| {
            if let Some(this) = weak.upgrade() {
                this.activate_window_action(window_action::OPEN_CLICK, parameter.cloned(), true);
            }
        });
        app.add_action(&open);

        let register = gio::SimpleAction::new(window_action::REGISTER_AGAIN, None);
        let weak = Rc::downgrade(self);
        register.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.activate_window_action(window_action::REGISTER_AGAIN, None, true);
            }
        });
        app.add_action(&register);

        let button = gio::SimpleAction::new(
            window_action::NOTIFICATION_ACTION,
            Some(glib::VariantTy::new("(ssss)").expect("valid type")),
        );
        let weak = Rc::downgrade(self);
        button.connect_activate(move |_, parameter| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            let Some((action, tag, title, message)) =
                parameter.and_then(|p| p.get::<(String, String, String, String)>())
            else {
                return;
            };
            notify::send_action(
                &this.app,
                &this.client,
                notify::action_event(&action, &tag, &title, &message),
            );
        });
        app.add_action(&button);
    }

    fn start(self: &Rc<Self>) {
        self.hold.replace(Some(self.app.hold()));
        let weak = Rc::downgrade(self);
        let appeared = move |_: gio::DBusConnection, _: &str, _: &str| {
            if let Some(this) = weak.upgrade() {
                this.window_app.set(true);
            }
        };
        let weak = Rc::downgrade(self);
        let vanished = move |_: gio::DBusConnection, _: &str| {
            if let Some(this) = weak.upgrade() {
                this.window_app.set(false);
                this.check_lifetime();
            }
        };
        // The watch lasts as long as the process.
        let _ = gio::bus_watch_name(
            gio::BusType::Session,
            APP_ID,
            gio::BusNameWatcherFlags::NONE,
            appeared,
            vanished,
        );
        self.reload();
    }

    /// Without Run in Background, the service lasts only as long as the
    /// window app.
    fn check_lifetime(&self) {
        if !self.window_app.get() && !self.settings.boolean("run-in-background") {
            log::info!("Stopping: the window app closed and Run in Background is off");
            self.stop();
        }
    }

    fn stop(&self) {
        self.generation.set(self.generation.get() + 1);
        self.reregistration.cancel();
        self.agent.pause();
        self.notifier().pause();
        self.client.set_credentials(None);
        self.hold.replace(None);
        self.app.quit();
    }

    fn reload(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let this = self.clone();
        glib::spawn_future_local(async move { this.load(generation).await });
    }

    async fn load(self: &Rc<Self>, generation: u64) {
        if !self.server.is_signed_in(&self.settings) || !self.server.is_companion() {
            log::info!("Stopping: not signed in as a companion");
            self.stop();
            return;
        }
        let url = self.server.get().string("url");
        let Ok(server) = ha::ServerUrl::parse(&url) else {
            log::warn!("Stored server address is invalid: {url}");
            self.stop();
            return;
        };
        let mut attempt = 1;
        let credentials = loop {
            match ha::credentials::load(&server).await {
                Ok(credentials) => break credentials,
                Err(err) if attempt < KEYRING_ATTEMPTS => {
                    log::info!("Could not read credentials, trying again: {err}");
                    attempt += 1;
                    glib::timeout_future(KEYRING_RETRY).await;
                    if self.generation.get() != generation {
                        return;
                    }
                }
                Err(err) => {
                    log::warn!("Could not read credentials: {err}");
                    break None;
                }
            }
        };
        if self.generation.get() != generation {
            return;
        }
        let Some(credentials) = credentials.filter(|c| c.registration.is_some()) else {
            log::info!("Stopping: no stored registration for {server}");
            self.stop();
            return;
        };
        let webhook = |c: &ha::Credentials| c.registration.as_ref().map(|r| r.webhook_id.clone());
        let previous = self.client.credentials();
        let replaced = previous
            .as_ref()
            .is_some_and(|previous| webhook(previous) != webhook(&credentials));
        self.client
            .set_endpoints(self.server.endpoints(credentials.registration.as_ref()));
        self.client.set_credentials(Some(credentials));
        self.clear_alert();
        self.agent.start();
        if replaced {
            self.agent.resend_all();
        }
        self.notifier().start();
    }

    fn on_push(self: &Rc<Self>, event: Event) {
        match event {
            Event::RegistrationLost => self.registration_lost(),
            Event::RequestLocation => self.agent.request_location_update(),
            Event::Command(command) => {
                if !control::allowed(&self.settings, &command) {
                    log::info!("Device command turned off in Preferences: {command:?}");
                    return;
                }
                let this = self.clone();
                glib::spawn_future_local(async move {
                    let forward = |forward: Forward| match forward {
                        Forward::Open(click) => this.activate_window_action(
                            window_action::OPEN_CLICK,
                            Some(click.target().to_variant()),
                            true,
                        ),
                        Forward::StopPlayback => {
                            this.activate_window_action(window_action::STOP_PLAYBACK, None, false)
                        }
                    };
                    control::run(&this.agent, command, forward).await;
                });
            }
        }
    }

    /// For a webhook or notification subscription that Home Assistant no
    /// longer recognizes.
    fn registration_lost(self: &Rc<Self>) {
        let Some(generation) = self.reregistration.begin() else {
            return;
        };
        log::warn!("Home Assistant no longer has this device's registration; registering again");
        self.update_status();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let name = this.server.get().string("device-name");
            let registration = this
                .reregistration
                .run(generation, &this.client, || {
                    ha::registration::register_device(&this.client, &name)
                })
                .await;
            match registration {
                Some(registration) => {
                    this.client.replace_registration(registration).await;
                    this.agent.resend_all();
                    this.notifier().resubscribe();
                }
                None if this.reregistration.gave_up() => this.show_alert().await,
                None => {}
            }
            this.update_status();
        });
    }

    async fn show_alert(&self) {
        self.alert.set_state(&true.to_variant());
        if self.window_shown().await {
            return;
        }
        let notification = gio::Notification::new(&gettext("Not Registered in Home Assistant"));
        notification.set_body(Some(&gettext(
            "Notifications and sensor updates are off until this device is registered again.",
        )));
        notification.add_button(
            &gettext("Register Again"),
            &format!("app.{}", window_action::REGISTER_AGAIN),
        );
        self.app
            .send_notification(Some(REGISTRATION_NOTIFICATION), &notification);
    }

    fn clear_alert(&self) {
        if self.alert.state().and_then(|s| s.get::<bool>()) == Some(true) {
            self.alert.set_state(&false.to_variant());
            self.app.withdraw_notification(REGISTRATION_NOTIFICATION);
            self.update_status();
        }
    }

    /// The line shown for Sanctum in the desktop's list of background apps.
    fn status_message(&self) -> Option<String> {
        if self.reregistration.gave_up() {
            return Some(gettext("Not registered in Home Assistant"));
        }
        let name = self.client.location_name();
        let name = if name.is_empty() {
            gettext("Home Assistant")
        } else {
            name
        };
        let template = if self.reregistration.running() {
            gettext("Registering again with {}")
        } else {
            match self.client.state() {
                ha::ConnectionState::Idle => return None,
                ha::ConnectionState::Connecting | ha::ConnectionState::Disconnected
                    if self.reconnecting.get() =>
                {
                    gettext("Reconnecting to {}")
                }
                ha::ConnectionState::Connecting | ha::ConnectionState::Disconnected => {
                    gettext("Connecting to {}")
                }
                ha::ConnectionState::Connected => gettext("Connected to {}"),
                ha::ConnectionState::AuthFailed => gettext("{} rejected the sign-in"),
            }
        };
        Some(template.replace("{}", &name))
    }

    fn update_status(&self) {
        if self.status_unsupported.get() {
            return;
        }
        let Some(message) = self.status_message() else {
            return;
        };
        if *self.status.borrow() == message {
            return;
        }
        self.status.replace(message.clone());
        let unsupported = self.status_unsupported.clone();
        glib::spawn_future_local(async move {
            if let Err(err) = crate::background::set_status(&message).await {
                log::info!("Could not set the background status: {err}");
                unsupported.set(true);
            }
        });
    }

    /// Asks the window app without starting it.
    async fn window_shown(&self) -> bool {
        if !self.window_app.get() {
            return false;
        }
        let Some(connection) = self.app.dbus_connection() else {
            return false;
        };
        let reply = connection
            .call_future(
                Some(APP_ID),
                &object_path(APP_ID),
                "org.gtk.Actions",
                "Describe",
                Some(&(window_action::WINDOW_SHOWN,).to_variant()),
                Some(glib::VariantTy::new("((bgav))").expect("valid type")),
                gio::DBusCallFlags::NO_AUTO_START,
                -1,
            )
            .await;
        // The reply is ((enabled, parameter type, [state])).
        reply
            .ok()
            .and_then(|reply| reply.child_value(0).child_value(2).try_child_value(0))
            .and_then(|state| state.as_variant())
            .and_then(|state| state.get::<bool>())
            .unwrap_or(false)
    }

    /// `start` lets D-Bus start the window app when it is not running.
    fn activate_window_action(&self, name: &str, parameter: Option<glib::Variant>, start: bool) {
        if !start && !self.window_app.get() {
            return;
        }
        let Some(connection) = self.app.dbus_connection() else {
            return;
        };
        let flags = if start {
            gio::DBusCallFlags::NONE
        } else {
            gio::DBusCallFlags::NO_AUTO_START
        };
        let parameters: Vec<glib::Variant> = parameter.into_iter().collect();
        let platform_data = glib::VariantDict::new(None).end();
        let name = name.to_owned();
        glib::spawn_future_local(async move {
            let result = connection
                .call_future(
                    Some(APP_ID),
                    &object_path(APP_ID),
                    "org.freedesktop.Application",
                    "ActivateAction",
                    Some(&glib::Variant::tuple_from_iter([
                        name.to_variant(),
                        parameters.to_variant(),
                        platform_data.clone(),
                    ])),
                    None,
                    flags,
                    -1,
                )
                .await;
            if let Err(err) = result {
                log::warn!("Could not reach the window app for {name}: {err}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_paths() {
        assert_eq!(
            object_path("net.dunevent.Sanctum.Service"),
            "/net/dunevent/Sanctum/Service"
        );
        assert_eq!(object_path("org.example.App-Name"), "/org/example/App_Name");
    }
}
