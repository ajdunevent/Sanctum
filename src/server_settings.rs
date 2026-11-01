//! Settings that belong to one Home Assistant server. Each server has a copy
//! of the Server schema at `/net/dunevent/Sanctum/servers/ID/`, so a
//! second server can be added later without moving anything.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

use crate::config::APP_ID;

/// Keys that were app-wide before servers had their own settings, with the
/// name each has in the Server schema.
const MOVED_KEYS: [(&str, &str); 11] = [
    ("server-url", "url"),
    ("internal-url", "internal-url"),
    ("external-url", "external-url"),
    ("home-networks", "home-networks"),
    ("device-name", "device-name"),
    ("last-path", "last-path"),
    ("accent-color", "accent-color"),
    ("assist-default-pipeline", "assist-default-pipeline"),
    ("assist-last-pipeline", "assist-last-pipeline"),
    ("disabled-sensors", "disabled-sensors"),
    ("location-enabled", "location-enabled"),
];

/// Server IDs end up in a settings path, so only letters and digits are used.
pub fn new_id() -> String {
    glib::uuid_string_random()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect()
}

fn open(id: &str) -> gio::Settings {
    let id = if id.is_empty() { "none" } else { id };
    gio::Settings::with_path(
        &format!("{APP_ID}.Server"),
        &format!("/net/dunevent/Sanctum/servers/{id}/"),
    )
}

type Watcher = Rc<dyn Fn(&gio::Settings)>;

struct Watch {
    keys: Vec<&'static str>,
    f: Watcher,
    handlers: Vec<glib::SignalHandlerId>,
}

/// The current server's settings. Watches survive a switch to another server.
pub struct ServerSettings {
    current: RefCell<gio::Settings>,
    watches: RefCell<Vec<Watch>>,
}

impl std::fmt::Debug for ServerSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerSettings").finish_non_exhaustive()
    }
}

impl ServerSettings {
    /// Follows the app's `server` key. Settings saved before servers had
    /// their own are moved into a new server on first start.
    pub fn new(app: &gio::Settings) -> Rc<Self> {
        migrate(app);
        let this = Rc::new(Self {
            current: RefCell::new(open(&app.string("server"))),
            watches: RefCell::default(),
        });
        let weak = Rc::downgrade(&this);
        app.connect_changed(Some("server"), move |app, key| {
            if let Some(this) = weak.upgrade() {
                this.switch(&app.string(key));
            }
        });
        this
    }

    pub fn get(&self) -> gio::Settings {
        self.current.borrow().clone()
    }

    /// Runs `f` when any of `keys` changes on the current server, and when
    /// the current server changes.
    pub fn watch(&self, keys: &[&'static str], f: impl Fn(&gio::Settings) + 'static) {
        let f: Watcher = Rc::new(f);
        let settings = self.get();
        let handlers = connect(&settings, keys, &f);
        self.watches.borrow_mut().push(Watch {
            keys: keys.to_vec(),
            f,
            handlers,
        });
    }

    fn switch(&self, id: &str) {
        let old = self.current.replace(open(id));
        let new = self.get();
        let mut watches = self.watches.borrow_mut();
        for watch in watches.iter_mut() {
            for handler in watch.handlers.drain(..) {
                old.disconnect(handler);
            }
            watch.handlers = connect(&new, &watch.keys, &watch.f);
        }
        let callbacks: Vec<Watcher> = watches.iter().map(|watch| watch.f.clone()).collect();
        drop(watches);
        for f in callbacks {
            f(&new);
        }
    }

    /// Makes `url` the current server, keeping its settings when it already
    /// is the current one.
    pub fn sign_in(&self, app: &gio::Settings, url: &str, device_name: &str, companion: bool) {
        let current = self.get();
        let server = if app.string("server").is_empty() || current.string("url") != url {
            let id = new_id();
            let server = open(&id);
            let _ = server.set_string("url", url);
            let _ = server.set_string("device-name", device_name);
            let _ = app.set_string("server", &id);
            server
        } else {
            if !device_name.is_empty() {
                let _ = current.set_string("device-name", device_name);
            }
            current
        };
        let _ = server.set_string("app-mode", mode_name(companion));
    }

    /// Clears everything stored for the current server.
    pub fn clear(&self) {
        let settings = self.get();
        if let Some(schema) = settings.settings_schema() {
            for key in schema.list_keys() {
                settings.reset(&key);
            }
        }
    }
}

pub fn mode_name(companion: bool) -> &'static str {
    if companion { "companion" } else { "frontend" }
}

fn connect(
    settings: &gio::Settings,
    keys: &[&'static str],
    f: &Watcher,
) -> Vec<glib::SignalHandlerId> {
    keys.iter()
        .map(|key| {
            let f = f.clone();
            settings.connect_changed(Some(key), move |settings, _| f(settings))
        })
        .collect()
}

fn migrate(app: &gio::Settings) {
    let url = app.string("server-url");
    if url.is_empty() || !app.string("server").is_empty() {
        return;
    }
    let id = new_id();
    let server = open(&id);
    for (old, new) in MOVED_KEYS {
        if let Some(value) = app.user_value(old) {
            let _ = server.set_value(new, &value);
        }
        app.reset(old);
    }
    let _ = app.set_string("server", &id);
    log::info!("Moved the settings for {url} to server {id}");
}
