use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::json;

use super::common::{Choice, Slider, Toggle, call, group, slider_row, watch};
use crate::model::Entity;

const SET_SPEED: u64 = 1;
const OSCILLATE: u64 = 2;
const PRESET_MODE: u64 = 8;

pub fn build(entity: &Entity) -> gtk::Widget {
    let controls = group("");

    let power = Toggle::new(
        &gettext("Power"),
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
    controls.add(&power.row);

    let speed = entity.supports(SET_SPEED).then(|| {
        let step = entity
            .attribute_f64("percentage_step")
            .unwrap_or(1.0)
            .max(1.0);
        let slider = Slider::new(
            0.0,
            100.0,
            step,
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |value| call(
                    &controls,
                    &entity,
                    "set_percentage",
                    json!({ "percentage": value.round() })
                )
            ),
        )
        .with_unit("%");
        controls.add(&slider_row(&gettext("Speed"), &slider));
        slider
    });

    let oscillate = entity.supports(OSCILLATE).then(|| {
        let toggle = Toggle::new(
            &gettext("Oscillate"),
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |on| call(
                    &controls,
                    &entity,
                    "oscillate",
                    json!({ "oscillating": on })
                )
            ),
        );
        controls.add(&toggle.row);
        toggle
    });

    let preset = entity.supports(PRESET_MODE).then(|| {
        let choice = Choice::new(
            &gettext("Preset"),
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |mode| call(
                    &controls,
                    &entity,
                    "set_preset_mode",
                    json!({ "preset_mode": mode })
                )
            ),
        );
        controls.add(&choice.row);
        choice
    });

    watch(entity, &controls, move |entity| {
        power.set(entity.raw_state() == "on");
        if let (Some(slider), Some(value)) = (&speed, entity.attribute_f64("percentage")) {
            slider.set(value);
        }
        if let Some(toggle) = &oscillate {
            toggle.set(
                entity
                    .attribute("oscillating")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            );
        }
        if let Some(choice) = &preset {
            choice.set(
                entity.attribute_strings("preset_modes"),
                entity.attribute_str("preset_mode").as_deref(),
            );
        }
    });
    controls.upcast()
}
