use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;
use serde_json::Value;

use super::present;
use crate::ha::registry::EntityEntry;
use crate::ha::states::EntityState;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Entity)]
    pub struct Entity {
        #[property(get, construct_only)]
        pub(super) entity_id: RefCell<String>,
        #[property(get)]
        pub(super) name: RefCell<String>,
        #[property(get)]
        pub(super) state_text: RefCell<String>,
        #[property(get)]
        pub(super) icon_name: RefCell<String>,
        #[property(get)]
        pub(super) active: Cell<bool>,
        #[property(get)]
        pub(super) available: Cell<bool>,

        pub(super) state: RefCell<EntityState>,
        pub(super) entry: RefCell<EntityEntry>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Entity {
        const NAME: &'static str = "SanctumEntity";
        type Type = super::Entity;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Entity {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            // Emitted on every state update, including attribute-only changes that
            // no property reflects.
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("updated").build()])
        }
    }
}

glib::wrapper! {
    pub struct Entity(ObjectSubclass<imp::Entity>);
}

impl Entity {
    pub fn new(entity_id: &str) -> Self {
        glib::Object::builder()
            .property("entity-id", entity_id)
            .build()
    }

    pub fn domain(&self) -> String {
        self.entity_id()
            .split_once('.')
            .map(|(domain, _)| domain.to_owned())
            .unwrap_or_default()
    }

    pub fn raw_state(&self) -> String {
        self.imp().state.borrow().state.clone()
    }

    pub fn attribute(&self, key: &str) -> Option<Value> {
        self.imp().state.borrow().attributes.get(key).cloned()
    }

    /// Unix seconds.
    pub fn last_changed(&self) -> f64 {
        self.imp().state.borrow().last_changed
    }

    pub fn entry(&self) -> EntityEntry {
        self.imp().entry.borrow().clone()
    }

    /// Recomputes the displayed values and notifies only the ones that changed.
    pub fn update(&self, state: &EntityState, entry: &EntityEntry) {
        let imp = self.imp();
        imp.state.replace(state.clone());
        imp.entry.replace(entry.clone());

        let domain = self.domain();
        let attributes = &state.attributes;
        let name = attributes
            .get("friendly_name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| self.entity_id());
        let device_class = attributes.get("device_class").and_then(Value::as_str);
        let explicit_icon = attributes
            .get("icon")
            .and_then(Value::as_str)
            .or(entry.icon.as_deref());

        self.set_string(&imp.name, name, "name");
        self.set_string(
            &imp.state_text,
            present::state_text(&domain, &state.state, attributes),
            "state-text",
        );
        self.set_string(
            &imp.icon_name,
            present::icon_name(&domain, &state.state, device_class, explicit_icon),
            "icon-name",
        );
        if imp
            .active
            .replace(present::is_active(&domain, &state.state))
            != imp.active.get()
        {
            self.notify_active();
        }
        if imp.available.replace(present::is_available(&state.state)) != imp.available.get() {
            self.notify_available();
        }
        self.emit_by_name::<()>("updated", &[]);
    }

    pub fn connect_updated<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("updated", false, move |values| {
            let entity = values[0]
                .get::<Self>()
                .expect("signal emitter is an Entity");
            f(&entity);
            None
        })
    }

    pub fn attribute_f64(&self, key: &str) -> Option<f64> {
        self.attribute(key).and_then(|value| value.as_f64())
    }

    pub fn attribute_str(&self, key: &str) -> Option<String> {
        self.attribute(key)
            .and_then(|value| value.as_str().map(str::to_owned))
    }

    pub fn attribute_strings(&self, key: &str) -> Vec<String> {
        self.attribute(key)
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    }

    pub fn supports(&self, feature: u64) -> bool {
        self.attribute("supported_features")
            .and_then(|value| value.as_u64())
            .is_some_and(|features| features & feature != 0)
    }

    fn set_string(&self, cell: &RefCell<String>, value: String, property: &str) {
        if *cell.borrow() != value {
            cell.replace(value);
            self.notify(property);
        }
    }
}
