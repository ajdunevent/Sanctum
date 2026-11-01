//! The Sensors page of Preferences: location, polling interval, and one switch
//! per sensor with its current value.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;

use super::permission_help::{explain, suffix_button};
use crate::agent::location::Status;
use crate::agent::sensor::{Category, DEFINITIONS};
use crate::application::Application;
use crate::control::sandbox::Policy;
use crate::model::present;

const ACCURACIES: [&str; 3] = ["city", "street", "exact"];

pub fn build(app: &Application) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Sensors"))
        .icon_name("mdi-gauge-symbolic")
        .build();
    page.add(&location_group(app));
    page.add(&reporting_group(app));
    sensor_groups(app, &page);
    page
}

fn location_group(app: &Application) -> adw::PreferencesGroup {
    let settings = app.settings();
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Location"))
        .description(gettext(
            "Sends this device's position to its device tracker in Home Assistant, for zones and presence. A new position is also sent when the network changes.",
        ))
        .build();

    let enabled = adw::SwitchRow::builder()
        .title(gettext("Report Location"))
        .build();
    app.server()
        .bind("location-enabled", &enabled, "active")
        .build();
    group.add(&enabled);

    let accuracy = adw::ComboRow::builder()
        .title(gettext("Accuracy"))
        .model(&gtk::StringList::new(&[
            &gettext("City"),
            &gettext("Street"),
            &gettext("Exact"),
        ]))
        .build();
    let current = settings.string("location-accuracy");
    accuracy.set_selected(
        ACCURACIES
            .iter()
            .position(|value| *value == current.as_str())
            .unwrap_or(1) as u32,
    );
    accuracy.connect_selected_notify(glib::clone!(
        #[weak]
        settings,
        move |row| {
            if let Some(value) = ACCURACIES.get(row.selected() as usize) {
                let _ = settings.set_string("location-accuracy", value);
            }
        }
    ));
    group.add(&accuracy);

    let interval = adw::SpinRow::with_range(1.0, 60.0, 1.0);
    interval.set_title(&gettext("Update Interval"));
    interval.set_subtitle(&gettext("Minutes between updates while moving"));
    interval.set_value((settings.uint("location-interval") / 60).max(1) as f64);
    interval.connect_value_notify(glib::clone!(
        #[weak]
        settings,
        move |row| {
            let _ = settings.set_uint("location-interval", row.value() as u32 * 60);
        }
    ));
    group.add(&interval);

    let status = adw::ActionRow::builder()
        .title(gettext("Status"))
        .css_classes(["property"])
        .build();
    group.add(&status);

    for row in [accuracy.upcast_ref::<gtk::Widget>(), interval.upcast_ref()] {
        enabled
            .bind_property("active", row, "sensitive")
            .sync_create()
            .build();
    }

    let agent = app.agent().clone();
    let update = glib::clone!(
        #[weak]
        status,
        #[weak]
        agent,
        move || status.set_subtitle(&location_status(&agent))
    );
    update();
    watch_agent(app, &group, update);
    group
}

fn location_status(agent: &crate::agent::Agent) -> String {
    match agent.location_status() {
        Status::Off => gettext("Off"),
        Status::Starting => gettext("Starting…"),
        Status::Failed(message) => message,
        Status::Running => match agent.location_sent_at() {
            0 => gettext("Waiting for a position"),
            sent => gettext("Last reported %s").replacen("%s", &present::ago(sent as f64), 1),
        },
    }
}

fn reporting_group(app: &Application) -> adw::PreferencesGroup {
    let settings = app.settings();
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Reporting"))
        .description(gettext(
            "Most sensors report as soon as they change. CPU and memory use are read on an interval, paused while the screen is off.",
        ))
        .build();
    let interval = adw::SpinRow::with_range(15.0, 3600.0, 15.0);
    interval.set_title(&gettext("Polling Interval"));
    interval.set_subtitle(&gettext("Seconds"));
    settings.bind("sensor-interval", &interval, "value").build();
    group.add(&interval);
    group
}

/// One group per category, filled as sensors become available. Rows are kept
/// and updated in place so a toggle does not rebuild the list under the user.
fn sensor_groups(app: &Application, page: &adw::PreferencesPage) {
    let groups: HashMap<Category, adw::PreferencesGroup> = Category::ALL
        .iter()
        .map(|category| {
            let group = adw::PreferencesGroup::builder()
                .title(category.title())
                .visible(false)
                .build();
            page.add(&group);
            (*category, group)
        })
        .collect();
    let waiting = adw::PreferencesGroup::builder()
        .title(gettext("Device Sensors"))
        .build();
    waiting.add(
        &adw::ActionRow::builder()
            .title(gettext("Sensors appear once Sanctum is connected"))
            .build(),
    );
    page.add(&waiting);

    // Set while rows show the agent's state, so their handlers do not write
    // the setting back.
    let quiet = Rc::new(Cell::new(false));
    let agent = app.agent().clone();
    let policy = Policy::current();
    let mut rows: HashMap<&'static str, adw::SwitchRow> = HashMap::new();
    for definition in DEFINITIONS {
        let group = &groups[&definition.category];
        if let Some(permission) = definition.permission().filter(|p| !policy.allows(*p)) {
            let row = adw::ActionRow::builder()
                .title(definition.name())
                .subtitle(gettext("Not available in this environment"))
                .build();
            let help = suffix_button("mdi-help-circle-outline-symbolic", &gettext("How to Allow"));
            help.set_visible(true);
            let title = definition.name();
            help.connect_clicked(move |button| explain(button, &title, permission));
            row.add_suffix(&help);
            group.add(&row);
            group.set_visible(true);
            continue;
        }
        // Rows stay hidden until a reading arrives, keeping definition order.
        let row = adw::SwitchRow::builder()
            .title(definition.name())
            .visible(false)
            .build();
        let id = definition.id;
        row.connect_active_notify(glib::clone!(
            #[weak]
            agent,
            #[strong]
            quiet,
            move |row| {
                if !quiet.get() {
                    agent.set_enabled(id, row.is_active());
                }
            }
        ));
        group.add(&row);
        rows.insert(definition.id, row);
    }
    let update = glib::clone!(
        #[weak]
        agent,
        #[weak]
        waiting,
        move || {
            let readings = agent.readings();
            waiting.set_visible(readings.is_empty());
            for reading in readings {
                let Some(row) = rows.get(reading.id) else {
                    continue;
                };
                row.set_visible(true);
                groups[&reading.definition().category].set_visible(true);
                row.set_subtitle(&glib::markup_escape_text(&reading.display()));
                quiet.set(true);
                row.set_active(agent.is_enabled(reading.id));
                quiet.set(false);
            }
        }
    );
    update();
    watch_agent(app, page, update);
}

/// Runs `f` on every agent change until `widget` is destroyed.
fn watch_agent(app: &Application, widget: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    let agent = app.agent().clone();
    let handler = RefCell::new(Some(agent.connect_changed(move |_| f())));
    widget.connect_destroy(move |_| {
        if let Some(handler) = handler.take() {
            agent.disconnect(handler);
        }
    });
}
