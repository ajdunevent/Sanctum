//! Preferences rows for binding the FuriOS assistant button to Assist, shown
//! only when the button daemon is running.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;

use crate::agent::dbus;
use crate::application::Application;
use crate::furios::{self, Action, Gesture, Unbound};

const VOICE_KEY: &str = "assistant-button-voice";

fn settings() -> Option<gtk::gio::Settings> {
    gtk::gio::Application::default()
        .and_downcast::<Application>()
        .map(|app| app.settings().clone())
}

fn wants_voice() -> bool {
    settings().is_some_and(|settings| settings.boolean(VOICE_KEY))
}

struct Rows {
    group: adw::PreferencesGroup,
    choice: adw::ComboRow,
    gestures: Vec<(Gesture, adw::ActionRow)>,
    test: adw::ActionRow,
    /// Set while the combo row shows the files' state, so its handler does
    /// not treat that as a user choice.
    quiet: Cell<bool>,
    /// The gesture awaiting a test press after binding.
    testing: Cell<Option<Gesture>>,
    presses: RefCell<Option<gtk::gio::SignalSubscription>>,
}

pub fn setup(group: &adw::PreferencesGroup) {
    let group = group.clone();
    glib::spawn_future_local(async move {
        if furios::is_available().await {
            build(&group).await;
        }
    });
}

fn choice_index(gesture: Option<Gesture>) -> u32 {
    match gesture {
        None => 0,
        Some(gesture) => Gesture::ALL.iter().position(|g| *g == gesture).unwrap_or(0) as u32 + 1,
    }
}

fn bound_gesture() -> Option<Gesture> {
    let directory = furios::directory();
    Gesture::ALL
        .into_iter()
        .find(|gesture| furios::current(&directory, *gesture) == Action::Assist)
}

async fn build(group: &adw::PreferencesGroup) {
    let mut labels = vec![gettext("Not Bound")];
    labels.extend(Gesture::ALL.iter().map(|gesture| gesture.title()));
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let choice = adw::ComboRow::builder()
        .title(gettext("Open Assist With"))
        .model(&gtk::StringList::new(&label_refs))
        .build();
    group.add(&choice);
    let voice = adw::SwitchRow::builder()
        .title(gettext("Always Use Voice"))
        .subtitle(gettext("Start listening when the button opens Assist"))
        .build();
    group.add(&voice);
    if let Some(settings) = settings() {
        settings.bind(VOICE_KEY, &voice, "active").build();
    }
    voice.connect_active_notify(|row| {
        let Some(gesture) = bound_gesture() else {
            return;
        };
        if let Err(err) = furios::bind(&furios::directory(), gesture, row.is_active()) {
            log::warn!("Could not update the assistant button: {err}");
        }
    });
    let gestures: Vec<(Gesture, adw::ActionRow)> = Gesture::ALL
        .into_iter()
        .map(|gesture| {
            let row = adw::ActionRow::builder()
                .title(gesture.title())
                .css_classes(["property"])
                .build();
            group.add(&row);
            (gesture, row)
        })
        .collect();
    let test = adw::ActionRow::builder()
        .title(gettext("Test"))
        .visible(false)
        .build();
    group.add(&test);
    group.set_visible(true);

    let rows = Rc::new(Rows {
        group: group.clone(),
        choice,
        gestures,
        test,
        quiet: Cell::new(false),
        testing: Cell::new(None),
        presses: RefCell::default(),
    });
    refresh(&rows);

    rows.choice.connect_selected_notify({
        let rows = Rc::downgrade(&rows);
        move |row| {
            let Some(rows) = rows.upgrade() else { return };
            if rows.quiet.get() {
                return;
            }
            let wanted = row
                .selected()
                .checked_sub(1)
                .and_then(|index| Gesture::ALL.get(index as usize).copied());
            glib::spawn_future_local(async move { apply(&rows, wanted).await });
        }
    });

    if let Some(bus) = dbus::session().await {
        let weak = Rc::downgrade(&rows);
        let subscription = furios::watch_presses(&bus, move |action, event| {
            if let Some(rows) = weak.upgrade() {
                on_press(&rows, action, event);
            }
        });
        rows.presses.replace(Some(subscription));
    }
    // The rows live as long as the group; the press subscription goes with them.
    let keep = RefCell::new(Some(rows));
    group.connect_destroy(move |_| {
        keep.take();
    });
}

fn refresh(rows: &Rows) {
    let directory = furios::directory();
    for (gesture, row) in &rows.gestures {
        row.set_subtitle(&glib::markup_escape_text(
            &furios::current(&directory, *gesture).describe(),
        ));
    }
    rows.quiet.set(true);
    rows.choice.set_selected(choice_index(bound_gesture()));
    rows.quiet.set(false);
}

fn toast(rows: &Rows, message: &str) {
    if let Some(dialog) = rows
        .group
        .ancestor(adw::PreferencesDialog::static_type())
        .and_downcast::<adw::PreferencesDialog>()
    {
        dialog.add_toast(adw::Toast::new(message));
    }
}

async fn confirm(rows: &Rows, heading: &str, body: &str) -> bool {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("bind", &gettext("_Use for Assist")),
    ]);
    dialog.set_response_appearance("bind", adw::ResponseAppearance::Suggested);
    dialog.choose_future(Some(&rows.group)).await == "bind"
}

async fn apply(rows: &Rows, wanted: Option<Gesture>) {
    let directory = furios::directory();
    let previous = bound_gesture();
    if wanted == previous {
        return;
    }

    if let Some(gesture) = wanted {
        let existing = furios::current(&directory, gesture);
        // The daemon delays short presses by the double press window when a
        // double press action exists.
        let delayed =
            gesture == Gesture::Short && furios::current(&directory, Gesture::Double).is_set();
        if existing.is_set() || delayed {
            let mut body = Vec::new();
            if existing.is_set() {
                body.push(
                    gettext(
                        "This gesture currently does: %s. Sanctum will replace it with Assist.",
                    )
                    .replacen("%s", &existing.describe(), 1),
                );
            }
            if delayed {
                body.push(gettext(
                    "A double press action is also set, so the button waits briefly after each short press before opening Assist.",
                ));
            }
            let heading = gettext("Use %s for Assist?").replacen("%s", &gesture.title(), 1);
            if !confirm(rows, &heading, &body.join("\n\n")).await {
                refresh(rows);
                return;
            }
        }
    }

    if let Some(gesture) = previous {
        match furios::unbind(&directory, gesture) {
            Ok(result) => {
                if let Some(message) = unbound_message(&result) {
                    toast(rows, &message);
                }
            }
            Err(err) => {
                toast(
                    rows,
                    &format!("{}: {err}", gettext("Could not change the button")),
                );
                refresh(rows);
                return;
            }
        }
    }

    match wanted {
        Some(gesture) => match furios::bind(&directory, gesture, wants_voice()) {
            Ok(()) => {
                rows.testing.set(Some(gesture));
                rows.test.set_subtitle(
                    &gettext("Press the button with a %s to check it").replacen(
                        "%s",
                        &gesture.title().to_lowercase(),
                        1,
                    ),
                );
                rows.test.set_visible(true);
            }
            Err(err) => toast(
                rows,
                &format!("{}: {err}", gettext("Could not change the button")),
            ),
        },
        None => {
            rows.testing.set(None);
            rows.test.set_visible(false);
        }
    }
    refresh(rows);
}

fn on_press(rows: &Rows, action: i32, event: i32) {
    let Some(gesture) = rows.testing.get() else {
        return;
    };
    if event != gesture.event() {
        return;
    }
    if action == furios::CUSTOM_ACTION {
        rows.testing.set(None);
        rows.test
            .set_subtitle(&gettext("Working: the button opens Assist"));
    } else {
        rows.test.set_subtitle(&gettext(
            "The button ran a different action; check the gesture in Settings",
        ));
    }
}

/// The file can change between reading and removing it, such as when the
/// user edits it by hand while Preferences is open.
fn unbound_message(result: &Unbound) -> Option<String> {
    match result {
        Unbound::ChangedElsewhere => Some(gettext(
            "The gesture's command was changed outside Sanctum, so it was left as is",
        )),
        _ => None,
    }
}
