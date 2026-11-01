use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::json;

use super::common::{Slider, button_box, call, confirm, group, pill, slider_row, watch};
use crate::model::Entity;

const OPEN: u64 = 1;
const CLOSE: u64 = 2;
const SET_POSITION: u64 = 4;
const STOP: u64 = 8;
const SET_TILT_POSITION: u64 = 128;

/// Device classes where moving the cover opens the home to the outside.
fn needs_confirmation(entity: &Entity) -> bool {
    matches!(
        entity.attribute_str("device_class").as_deref(),
        Some("garage" | "door" | "gate")
    )
}

fn guarded(
    widget: &impl IsA<gtk::Widget>,
    entity: &Entity,
    service: &'static str,
    data: serde_json::Value,
) {
    let (widget, entity) = (widget.clone().upcast::<gtk::Widget>(), entity.clone());
    glib::spawn_future_local(async move {
        if needs_confirmation(&entity) {
            let heading = if service == "close_cover" {
                gettext("Close %s?")
            } else {
                gettext("Open %s?")
            }
            .replacen("%s", &entity.name(), 1);
            let (action, appearance) = if service == "close_cover" {
                (gettext("_Close"), adw::ResponseAppearance::Suggested)
            } else {
                (gettext("_Open"), adw::ResponseAppearance::Destructive)
            };
            if !confirm(&widget, &heading, &action, appearance).await {
                return;
            }
        }
        call(&widget, &entity, service, data);
    });
}

pub fn build(entity: &Entity) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 18);

    let mut buttons = Vec::new();
    for (feature, label, service) in [
        (OPEN, gettext("_Open"), "open_cover"),
        (STOP, gettext("_Stop"), "stop_cover"),
        (CLOSE, gettext("_Close"), "close_cover"),
    ] {
        if !entity.supports(feature) {
            continue;
        }
        let button = pill(&label);
        button.connect_clicked(glib::clone!(
            #[strong]
            entity,
            move |button| {
                if service == "stop_cover" {
                    call(button, &entity, service, json!({}));
                } else {
                    guarded(button, &entity, service, json!({}));
                }
            }
        ));
        buttons.push((button, service));
    }
    let refs: Vec<&gtk::Button> = buttons.iter().map(|(button, _)| button).collect();
    if !refs.is_empty() {
        column.append(&button_box(&refs));
    }

    let controls = group("");
    let position = entity.supports(SET_POSITION).then(|| {
        let slider = Slider::new(
            0.0,
            100.0,
            1.0,
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |value| guarded(
                    &controls,
                    &entity,
                    "set_cover_position",
                    json!({ "position": value.round() })
                )
            ),
        )
        .with_unit("%");
        controls.add(&slider_row(&gettext("Position"), &slider));
        slider
    });
    let tilt = entity.supports(SET_TILT_POSITION).then(|| {
        let slider = Slider::new(
            0.0,
            100.0,
            1.0,
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |value| call(
                    &controls,
                    &entity,
                    "set_cover_tilt_position",
                    json!({ "tilt_position": value.round() })
                )
            ),
        )
        .with_unit("%");
        controls.add(&slider_row(&gettext("Tilt"), &slider));
        slider
    });
    if position.is_some() || tilt.is_some() {
        column.append(&controls);
    }

    watch(entity, &column, move |entity| {
        if let (Some(slider), Some(value)) = (&position, entity.attribute_f64("current_position")) {
            slider.set(value);
        }
        if let (Some(slider), Some(value)) = (&tilt, entity.attribute_f64("current_tilt_position"))
        {
            slider.set(value);
        }
        let state = entity.raw_state();
        let fully_open = entity
            .attribute_f64("current_position")
            .is_none_or(|p| p >= 100.0);
        for (button, service) in &buttons {
            button.set_sensitive(match *service {
                "open_cover" => !(state == "open" && fully_open) && state != "opening",
                "close_cover" => state != "closed" && state != "closing",
                _ => true,
            });
        }
    });
    column.upcast()
}
