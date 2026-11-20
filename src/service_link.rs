//! The window app's connection to `sanctum-service`: starting it, telling it
//! the sign-in changed, and following the states it exports.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

use crate::agent::Snapshot;
use crate::service::{self, action};

type Handler = Rc<dyn Fn()>;

#[derive(Default)]
pub struct ServiceLink {
    group: RefCell<Option<gio::DBusActionGroup>>,
    handlers: RefCell<Vec<(usize, Handler)>>,
    next_handler: Cell<usize>,
    watching: Cell<bool>,
}

impl ServiceLink {
    pub fn new() -> Rc<Self> {
        Rc::default()
    }

    /// Follows the service while it is on the bus. A new process gets a new
    /// action group, so no state from an earlier one is kept.
    pub fn watch(self: &Rc<Self>) {
        if self.watching.replace(true) {
            return;
        }
        let id = service::id();
        let weak = Rc::downgrade(self);
        let appeared = move |connection: gio::DBusConnection, name: &str, _: &str| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            let group =
                gio::DBusActionGroup::get(&connection, Some(name), &service::object_path(name));
            let changed = Rc::downgrade(&this);
            group.connect_action_state_changed(None, move |_, _, _| {
                if let Some(this) = changed.upgrade() {
                    this.emit_changed();
                }
            });
            let added = Rc::downgrade(&this);
            group.connect_action_added(None, move |_, _| {
                if let Some(this) = added.upgrade() {
                    this.emit_changed();
                }
            });
            // Listing makes the group fetch the actions and their states.
            let _ = group.list_actions();
            this.group.replace(Some(group));
            this.emit_changed();
        };
        let weak = Rc::downgrade(self);
        let vanished = move |_: gio::DBusConnection, _: &str| {
            if let Some(this) = weak.upgrade()
                && this.group.take().is_some()
            {
                this.emit_changed();
            }
        };
        // The watch lasts as long as the process.
        let _ = gio::bus_watch_name(
            gio::BusType::Session,
            &id,
            gio::BusNameWatcherFlags::NONE,
            appeared,
            vanished,
        );
    }

    /// Runs `f` when the service appears or goes, or a state changes.
    pub fn connect_changed(&self, f: impl Fn() + 'static) -> usize {
        let id = self.next_handler.get();
        self.next_handler.set(id + 1);
        self.handlers.borrow_mut().push((id, Rc::new(f)));
        id
    }

    pub fn disconnect(&self, id: usize) {
        self.handlers
            .borrow_mut()
            .retain(|(handler, _)| *handler != id);
    }

    fn emit_changed(&self) {
        let handlers: Vec<Handler> = self
            .handlers
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        for handler in handlers {
            handler();
        }
    }

    fn state(&self, name: &str) -> Option<glib::Variant> {
        self.group.borrow().as_ref()?.action_state(name)
    }

    pub fn registration_alert(&self) -> bool {
        self.state(action::REGISTRATION_ALERT)
            .and_then(|state| state.get::<bool>())
            .unwrap_or(false)
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state(action::SENSORS)
            .and_then(|state| state.get::<String>())
            .map(|json| Snapshot::from_json(&json))
            .unwrap_or_default()
    }

    /// Starts the service if needed. It reads the sign-in when it starts.
    pub fn ensure(&self) {
        call(
            "org.freedesktop.Application",
            "Activate",
            glib::Variant::tuple_from_iter([platform_data()]),
            true,
        );
    }

    /// Makes the service read the sign-in again, starting it if needed. It
    /// stops itself when the device is no longer signed in as a companion.
    pub fn reload(&self) {
        activate(action::RELOAD, true);
    }

    /// Stops the service if it is running. Awaited before the window app
    /// quits, which would otherwise drop the call.
    pub async fn quit(&self) {
        call_future(
            "org.gtk.Actions",
            "Activate",
            activate_parameters(action::QUIT),
            false,
        )
        .await;
    }
}

fn activate(name: &str, start: bool) {
    call(
        "org.gtk.Actions",
        "Activate",
        activate_parameters(name),
        start,
    );
}

fn activate_parameters(name: &str) -> glib::Variant {
    let no_parameter: Vec<glib::Variant> = Vec::new();
    glib::Variant::tuple_from_iter([
        name.to_variant(),
        no_parameter.to_variant(),
        platform_data(),
    ])
}

/// An empty `a{sv}`.
fn platform_data() -> glib::Variant {
    glib::VariantDict::new(None).end()
}

/// `start` lets D-Bus start the service. Where it cannot, as in a development
/// build that is not installed, the service is started as a child process.
fn call(interface: &'static str, method: &'static str, parameters: glib::Variant, start: bool) {
    glib::spawn_future_local(call_future(interface, method, parameters, start));
}

async fn call_future(
    interface: &'static str,
    method: &'static str,
    parameters: glib::Variant,
    start: bool,
) {
    let connection = match gio::bus_get_future(gio::BusType::Session).await {
        Ok(connection) => connection,
        Err(err) => {
            log::warn!("No session bus for the service: {err}");
            return;
        }
    };
    let id = service::id();
    let flags = if start {
        gio::DBusCallFlags::NONE
    } else {
        gio::DBusCallFlags::NO_AUTO_START
    };
    let result = connection
        .call_future(
            Some(&id),
            &service::object_path(&id),
            interface,
            method,
            Some(&parameters),
            None,
            flags,
            -1,
        )
        .await;
    match result {
        Ok(_) => {}
        Err(err) if start && err.matches(gio::DBusError::ServiceUnknown) => {
            log::info!("Starting sanctum-service directly: {err}");
            if let Err(err) =
                gio::Subprocess::newv(&["sanctum-service".as_ref()], gio::SubprocessFlags::NONE)
            {
                log::warn!("Could not start sanctum-service: {err}");
            }
        }
        Err(err) if !start => log::debug!("Service not running for {method}: {err}"),
        Err(err) => log::warn!("Could not reach sanctum-service: {err}"),
    }
}
