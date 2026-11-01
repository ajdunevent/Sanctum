mod alarm;
mod climate;
mod common;
mod cover;
mod fan;
mod history;
mod light;
mod lock;
mod media;
mod simple;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;

use crate::model::{Entity, present};
pub use common::watch;

/// Domains whose state is a moment in time, where a history graph says nothing.
fn has_history(domain: &str) -> bool {
    !matches!(domain, "scene" | "script" | "button" | "input_button")
}

fn controls(entity: &Entity) -> Option<gtk::Widget> {
    match entity.domain().as_str() {
        "light" => Some(light::build(entity)),
        "fan" => Some(fan::build(entity)),
        "climate" => Some(climate::build(entity)),
        "cover" => Some(cover::build(entity)),
        "lock" => Some(lock::build(entity)),
        "media_player" => Some(media::build(entity)),
        "alarm_control_panel" => Some(alarm::build(entity)),
        _ => simple::build(entity),
    }
}

fn header(entity: &Entity) -> gtk::Widget {
    let icon = gtk::Image::builder()
        .pixel_size(48)
        .css_classes(["details-icon"])
        .build();
    let state = gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["title-2"])
        .build();
    let changed = gtk::Label::builder()
        .css_classes(["caption", "dim-label"])
        .build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.append(&icon);
    column.append(&state);
    column.append(&changed);

    watch(entity, &column, move |entity| {
        icon.set_icon_name(Some(&entity.icon_name()));
        if entity.active() {
            icon.add_css_class("active");
        } else {
            icon.remove_css_class("active");
        }
        state.set_label(&entity.state_text());
        changed.set_label(&since(entity.last_changed()));
        changed.set_visible(entity.last_changed() > 0.0);
    });
    column.upcast()
}

fn since(time: f64) -> String {
    gettext("Changed %s").replacen("%s", &present::ago(time), 1)
}

pub fn present(parent: &impl IsA<gtk::Widget>, entity: &Entity) {
    // Tiles are rebuilt when the store's structure changes, so the window
    // outlives the widget that opened the dialog.
    let parent = parent
        .root()
        .and_upcast::<gtk::Widget>()
        .unwrap_or_else(|| parent.clone().upcast());
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&header(entity));
    if let Some(controls) = controls(entity).filter(|_| entity.available()) {
        content.append(&controls);
    }
    if has_history(&entity.domain()) {
        content.append(&history::build(entity));
    }

    let clamp = adw::Clamp::builder()
        .maximum_size(520)
        .child(&content)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&clamp)
        .build();

    let title = adw::WindowTitle::new(&entity.name(), "");
    let bar = adw::HeaderBar::builder().title_widget(&title).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&bar);
    toolbar.set_content(Some(&scrolled));
    let overlay = adw::ToastOverlay::new();
    overlay.set_child(Some(&toolbar));

    let dialog = adw::Dialog::builder()
        .title(entity.name())
        .content_width(480)
        .child(&overlay)
        .build();
    watch(entity, &dialog, move |entity| {
        title.set_title(&entity.name())
    });

    // Rebuilding on availability changes keeps controls from acting on a
    // device that went away, and brings them back when it returns.
    let available = entity.available();
    let handler = entity.connect_notify_local(
        Some("available"),
        glib::clone!(
            #[weak]
            dialog,
            #[weak]
            parent,
            move |entity, _| {
                if entity.available() != available {
                    dialog.close();
                    present(&parent, entity);
                }
            }
        ),
    );
    let weak = entity.downgrade();
    let handler = std::cell::RefCell::new(Some(handler));
    dialog.connect_closed(move |_| {
        if let (Some(entity), Some(handler)) = (weak.upgrade(), handler.take()) {
            entity.disconnect(handler);
        }
    });

    // Focus is lost when a focused button is disabled by an update or an alert
    // returns focus to a disabled button, after which Escape stops working.
    dialog.connect_focus_widget_notify(|dialog| {
        if AdwDialogExt::focus(dialog).is_none() {
            glib::idle_add_local_once(glib::clone!(
                #[weak]
                dialog,
                move || {
                    if AdwDialogExt::focus(&dialog).is_none() {
                        dialog.child_focus(gtk::DirectionType::TabForward);
                    }
                }
            ));
        }
    });

    dialog.present(Some(&parent));
}
