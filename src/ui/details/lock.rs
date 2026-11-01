use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::{Value, json};

use super::common::{button_box, call, confirm, group, pill, watch};
use crate::model::Entity;

const OPEN: u64 = 1;

/// Service data carrying the entered code, if any. The row is cleared so the
/// code does not stay on screen after use.
pub fn take_code(code: Option<&adw::PasswordEntryRow>) -> Value {
    let Some(row) = code else { return json!({}) };
    let text = row.text();
    row.set_text("");
    if text.is_empty() {
        json!({})
    } else {
        json!({ "code": text.as_str() })
    }
}

pub fn code_row(entity: &Entity) -> Option<adw::PasswordEntryRow> {
    entity.attribute_str("code_format").map(|format| {
        let row = adw::PasswordEntryRow::builder()
            .title(gettext("Code"))
            .build();
        if format == "number" || format.starts_with("^\\d") {
            row.set_input_purpose(gtk::InputPurpose::Pin);
        }
        row
    })
}

pub fn build(entity: &Entity) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 18);
    let code = code_row(entity);
    if let Some(row) = &code {
        let codes = group("");
        codes.add(row);
        column.append(&codes);
    }

    let lock = pill(&gettext("_Lock"));
    let unlock = pill(&gettext("_Unlock"));
    let open = pill(&gettext("_Open"));
    open.set_visible(entity.supports(OPEN));
    unlock.add_css_class("destructive-action");

    lock.connect_clicked(glib::clone!(
        #[strong]
        entity,
        #[strong]
        code,
        move |button| call(button, &entity, "lock", take_code(code.as_ref()))
    ));
    for (button, service, heading, action) in [
        (&unlock, "unlock", gettext("Unlock %s?"), gettext("_Unlock")),
        (&open, "open", gettext("Open %s?"), gettext("_Open")),
    ] {
        button.connect_clicked(glib::clone!(
            #[strong]
            entity,
            #[strong]
            code,
            move |button| {
                let (button, entity, code) = (button.clone(), entity.clone(), code.clone());
                let heading = heading.replacen("%s", &entity.name(), 1);
                let action = action.clone();
                glib::spawn_future_local(async move {
                    if confirm(
                        &button,
                        &heading,
                        &action,
                        adw::ResponseAppearance::Destructive,
                    )
                    .await
                    {
                        call(&button, &entity, service, take_code(code.as_ref()));
                    }
                });
            }
        ));
    }
    column.append(&button_box(&[&lock, &unlock, &open]));

    watch(entity, &column, move |entity| {
        let state = entity.raw_state();
        lock.set_sensitive(!matches!(state.as_str(), "locked" | "locking"));
        unlock.set_sensitive(!matches!(state.as_str(), "unlocked" | "unlocking"));
    });
    column.upcast()
}
