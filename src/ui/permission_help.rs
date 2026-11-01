//! The help shared by the Controls and Sensors pages for anything the Flatpak
//! sandbox blocks.

use adw::prelude::*;
use gettextrs::gettext;

use crate::control::catalog;
use crate::control::sandbox::Permission;

pub fn suffix_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .visible(false)
        .build();
    crate::ui::label_by_tooltip(&button);
    button
}

fn toast(widget: &impl IsA<gtk::Widget>, message: &str) {
    if let Some(dialog) = widget
        .ancestor(adw::PreferencesDialog::static_type())
        .and_downcast::<adw::PreferencesDialog>()
    {
        dialog.add_toast(adw::Toast::new(message));
    }
}

pub fn copy_text(widget: &impl IsA<gtk::Widget>, text: &str, confirmation: &str) {
    widget.clipboard().set_text(text);
    toast(widget, confirmation);
}

fn override_command(permission: Permission) -> String {
    format!(
        "flatpak override --user {} {}",
        permission.flag(),
        crate::config::APP_ID
    )
}

pub fn explain(button: &gtk::Button, title: &str, permission: Permission) {
    let command = override_command(permission);
    let text = gtk::Label::builder()
        .label(&command)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .css_classes(["monospace"])
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let card = gtk::Box::builder()
        .css_classes(["card"])
        .margin_top(6)
        .build();
    card.append(&text);
    let flatseal = gtk::Label::builder()
        .label(gettext("Or, in Flatseal, select Sanctum and %s.").replacen(
            "%s",
            &catalog::flatseal_hint(permission),
            1,
        ))
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let extra = gtk::Box::new(gtk::Orientation::Vertical, 12);
    extra.append(&card);
    extra.append(&flatseal);

    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Allow %s").replacen("%s", title, 1))
        .body(
            gettext(
                "The Flatpak sandbox keeps Sanctum from reaching %s. To allow it, run this command, then quit Sanctum and open it again:",
            )
            .replacen("%s", &catalog::reaches(permission), 1),
        )
        .extra_child(&extra)
        .default_response("copy")
        .close_response("close")
        .build();
    dialog.add_responses(&[
        ("close", &gettext("_Close")),
        ("copy", &gettext("_Copy Command")),
    ]);
    dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
    let button_weak = button.downgrade();
    dialog.connect_response(Some("copy"), move |_, _| {
        if let Some(button) = button_weak.upgrade() {
            copy_text(&button, &command, &gettext("Command copied"));
        }
    });
    dialog.present(Some(button));
}
