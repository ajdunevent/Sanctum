use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

use futures_util::StreamExt;
use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;
use serde_json::json;

use super::entity::Entity;
use crate::ha::registry::{self, Registries};
use crate::ha::states::{self, EntityState};
use crate::ha::{Client, ConnectionState};

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Store)]
    pub struct Store {
        /// True once registries and the first state snapshot have arrived.
        #[property(get)]
        pub(super) loaded: Cell<bool>,

        pub(super) client: OnceCell<Client>,
        pub(super) states: RefCell<HashMap<String, EntityState>>,
        pub(super) entities: RefCell<HashMap<String, Entity>>,
        pub(super) registries: RefCell<Registries>,
        /// Bumped per connection so a stale event loop stops applying events.
        pub(super) generation: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Store {
        const NAME: &'static str = "SanctumStore";
        type Type = super::Store;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Store {}
}

glib::wrapper! {
    pub struct Store(ObjectSubclass<imp::Store>);
}

impl Store {
    pub fn new(client: &Client) -> Self {
        let store: Self = glib::Object::new();
        store
            .imp()
            .client
            .set(client.clone())
            .expect("client is set once");
        client.connect_state_notify(glib::clone!(
            #[weak]
            store,
            move |client| store.on_connection_changed(client.state())
        ));
        store
    }

    fn client(&self) -> &Client {
        self.imp().client.get().expect("client is set in new()")
    }

    fn on_connection_changed(&self, state: ConnectionState) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
        if state == ConnectionState::Connected {
            let generation = imp.generation.get();
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = store)]
                self,
                async move { store.load(generation).await }
            ));
        } else if imp.loaded.replace(false) {
            self.notify_loaded();
        }
    }

    async fn load(&self, generation: u64) {
        let imp = self.imp();
        let client = self.client().clone();
        let registries = match registry::load(&client).await {
            Ok(registries) => registries,
            Err(err) => {
                log::warn!("Could not load registries: {err}");
                return;
            }
        };
        if imp.generation.get() != generation {
            return;
        }
        imp.registries.replace(registries);

        let mut subscription = match client
            .subscribe(json!({ "type": "subscribe_entities" }))
            .await
        {
            Ok(subscription) => subscription,
            Err(err) => {
                log::warn!("Could not subscribe to entities: {err}");
                return;
            }
        };
        // Each connection starts from a fresh snapshot.
        imp.states.borrow_mut().clear();
        let mut first = true;
        while let Some(event) = subscription.events.next().await {
            if imp.generation.get() != generation {
                return;
            }
            let update = states::apply(&mut imp.states.borrow_mut(), &event);
            self.sync_entities(first, &update);
            if first {
                first = false;
                imp.loaded.set(true);
                self.notify_loaded();
            }
        }
    }

    fn sync_entities(&self, rebuild: bool, update: &states::Update) {
        let imp = self.imp();
        let states = imp.states.borrow();
        let registries = imp.registries.borrow();
        let mut entities = imp.entities.borrow_mut();
        if rebuild {
            entities.retain(|id, _| states.contains_key(id));
        } else {
            for id in &update.removed {
                entities.remove(id);
            }
        }

        let touched: Vec<&String> = if rebuild {
            states.keys().collect()
        } else {
            update.added.iter().chain(&update.changed).collect()
        };
        for id in touched {
            let Some(state) = states.get(id) else {
                continue;
            };
            let entry = registries.entities.get(id).cloned().unwrap_or_default();
            entities
                .entry(id.clone())
                .or_insert_with(|| Entity::new(id))
                .update(state, &entry);
        }
    }

    pub fn entity(&self, entity_id: &str) -> Option<Entity> {
        self.imp().entities.borrow().get(entity_id).cloned()
    }
}
