use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;

use super::permission_help::{copy_text, explain, suffix_button};
use crate::application::Application;
use crate::control::catalog::{self, Availability, Group, Kind};

pub fn build(app: &Application) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Controls"))
        .icon_name("mdi-cellphone-cog-symbolic")
        .build();
    let service = catalog::notify_service(&app.device_info().name);

    let top = adw::PreferencesGroup::builder()
        .description(
            gettext(
                "Automations control this device by sending these messages with the notify.%s action. The copy button puts an example on the clipboard.",
            )
            .replacen("%s", &service, 1),
        )
        .build();
    let allow = adw::SwitchRow::builder()
        .title(gettext("Allow Device Commands"))
        .build();
    app.settings()
        .bind("device-commands", &allow, "active")
        .build();
    top.add(&allow);
    page.add(&top);

    for group in Group::ALL {
        let section = adw::PreferencesGroup::builder()
            .title(group.title())
            .build();
        for kind in Kind::ALL.into_iter().filter(|kind| kind.group() == group) {
            section.add(&command_row(app, kind, &service, &allow));
        }
        page.add(&section);
    }
    page
}

fn command_row(
    app: &Application,
    kind: Kind,
    service: &str,
    allow: &adw::SwitchRow,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(kind.title())
        .subtitle(kind.message())
        .build();
    let copy = suffix_button("edit-copy-symbolic", &gettext("Copy Example"));
    let help = suffix_button("mdi-help-circle-outline-symbolic", &gettext("How to Allow"));
    let switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    row.add_suffix(&copy);
    row.add_suffix(&help);
    row.add_suffix(&switch);

    let row_weak = row.downgrade();
    let app = app.clone();
    let allow = allow.clone();
    let service = service.to_owned();
    glib::spawn_future_local(async move {
        let availability = kind.availability().await;
        let Some(row) = row_weak.upgrade() else {
            return;
        };
        match availability {
            Availability::Available => {
                enable(&app, kind, &row, &switch, &allow);
                copy.connect_clicked(move |button| {
                    copy_text(button, &kind.example(&service), &gettext("Example copied"));
                });
                copy.set_visible(true);
            }
            Availability::Missing(reason) => row.set_subtitle(&reason),
            Availability::Unavailable => {
                row.set_subtitle(&gettext("Not available in this environment"));
            }
            Availability::NeedsPermission(permission) => {
                row.set_subtitle(&gettext("Not available in this environment"));
                help.connect_clicked(move |button| explain(button, &kind.title(), permission));
                help.set_visible(true);
            }
        }
    });
    row
}

fn enable(
    app: &Application,
    kind: Kind,
    row: &adw::ActionRow,
    switch: &gtk::Switch,
    allow: &adw::SwitchRow,
) {
    let settings = app.settings().clone();
    let disabled = |settings: &gtk::gio::Settings| -> Vec<String> {
        settings
            .strv("disabled-commands")
            .iter()
            .map(|id| id.to_string())
            .collect()
    };
    switch.set_active(!disabled(&settings).iter().any(|id| id == kind.message()));
    allow
        .bind_property("active", switch, "sensitive")
        .sync_create()
        .build();
    row.set_activatable_widget(Some(switch));
    switch.connect_active_notify(move |switch| {
        let mut ids = disabled(&settings);
        ids.retain(|id| id != kind.message());
        if !switch.is_active() {
            ids.push(kind.message().to_owned());
        }
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let _ = settings.set_strv("disabled-commands", ids.as_slice());
    });
}
