use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::json;

use super::common::{Choice, Slider, Toggle, call, group, pill, slider_row, watch};
use crate::model::Entity;

/// Controls for domains with one action or value. Returns `None` for
/// read-only domains such as sensors.
pub fn build(entity: &Entity) -> Option<gtk::Widget> {
    match entity.domain().as_str() {
        "switch" | "input_boolean" | "siren" => Some(power(entity).upcast()),
        "automation" => Some(automation(entity)),
        "humidifier" => Some(humidifier(entity)),
        "scene" | "script" => Some(run(entity, &gettext("_Run"), "turn_on")),
        "button" | "input_button" => Some(run(entity, &gettext("_Press"), "press")),
        "input_select" | "select" => Some(select(entity)),
        "input_number" | "number" => Some(number(entity)),
        _ => None,
    }
}

fn power(entity: &Entity) -> adw::PreferencesGroup {
    let controls = group("");
    let title = if entity.domain() == "automation" {
        gettext("Enabled")
    } else {
        gettext("Power")
    };
    let toggle = Toggle::new(
        &title,
        glib::clone!(
            #[weak]
            controls,
            #[strong]
            entity,
            move |on| call(
                &controls,
                &entity,
                if on { "turn_on" } else { "turn_off" },
                json!({})
            )
        ),
    );
    controls.add(&toggle.row);
    watch(entity, &controls, move |entity| {
        toggle.set(entity.raw_state() == "on")
    });
    controls
}

fn humidifier(entity: &Entity) -> gtk::Widget {
    let controls = power(entity);
    let min = entity.attribute_f64("min_humidity").unwrap_or(0.0);
    let max = entity.attribute_f64("max_humidity").unwrap_or(100.0);
    let slider = Slider::new(
        min,
        max,
        1.0,
        glib::clone!(
            #[weak]
            controls,
            #[strong]
            entity,
            move |value| call(
                &controls,
                &entity,
                "set_humidity",
                json!({ "humidity": value.round() })
            )
        ),
    )
    .with_unit("%");
    controls.add(&slider_row(&gettext("Target Humidity"), &slider));
    watch(entity, &controls, move |entity| {
        if let Some(value) = entity.attribute_f64("humidity") {
            slider.set(value);
        }
    });
    controls.upcast()
}

/// The enable switch plus a way to run the actions without the trigger.
fn automation(entity: &Entity) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 18);
    column.append(&power(entity));
    column.append(&run(entity, &gettext("_Run Actions"), "trigger"));
    column.upcast()
}

fn run(entity: &Entity, label: &str, service: &'static str) -> gtk::Widget {
    let button = pill(label);
    button.add_css_class("suggested-action");
    button.set_halign(gtk::Align::Center);
    button.set_hexpand(false);
    button.connect_clicked(glib::clone!(
        #[strong]
        entity,
        move |button| call(button, &entity, service, json!({}))
    ));
    button.upcast()
}

fn select(entity: &Entity) -> gtk::Widget {
    let controls = group("");
    let choice = Choice::verbatim(
        &gettext("Option"),
        glib::clone!(
            #[weak]
            controls,
            #[strong]
            entity,
            move |option| call(
                &controls,
                &entity,
                "select_option",
                json!({ "option": option })
            )
        ),
    );
    controls.add(&choice.row);
    watch(entity, &controls, move |entity| {
        choice.set(
            entity.attribute_strings("options"),
            Some(&entity.raw_state()),
        );
    });
    controls.upcast()
}

fn number(entity: &Entity) -> gtk::Widget {
    let controls = group("");
    let min = entity.attribute_f64("min").unwrap_or(0.0);
    let max = entity.attribute_f64("max").unwrap_or(100.0);
    let step = entity.attribute_f64("step").unwrap_or(1.0);
    let slider = Slider::new(
        min,
        max,
        step,
        glib::clone!(
            #[weak]
            controls,
            #[strong]
            entity,
            move |value| call(&controls, &entity, "set_value", json!({ "value": value }))
        ),
    );
    if step.fract() != 0.0 {
        slider.scale.set_digits(
            step.to_string()
                .split('.')
                .nth(1)
                .map_or(1, |d| d.len() as i32),
        );
    }
    controls.add(&slider_row(&gettext("Value"), &slider));
    watch(entity, &controls, move |entity| {
        if let Ok(value) = entity.raw_state().parse() {
            slider.set(value);
        }
    });
    controls.upcast()
}
