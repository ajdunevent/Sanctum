use std::cell::OnceCell;
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::json;

use super::common::{Choice, Stepper, call, group, watch};
use crate::model::Entity;

const TARGET_TEMPERATURE: u64 = 1;
const TARGET_TEMPERATURE_RANGE: u64 = 2;
const FAN_MODE: u64 = 8;
const PRESET_MODE: u64 = 16;
const SWING_MODE: u64 = 32;

pub fn build(entity: &Entity) -> gtk::Widget {
    let controls = group("");
    let step = entity.attribute_f64("target_temp_step").unwrap_or(0.5);
    let min = entity.attribute_f64("min_temp").unwrap_or(7.0);
    let max = entity.attribute_f64("max_temp").unwrap_or(35.0);

    let current = adw::ActionRow::builder()
        .title(gettext("Current Temperature"))
        .css_classes(["property"])
        .build();
    controls.add(&current);

    let target = entity.supports(TARGET_TEMPERATURE).then(|| {
        let stepper = Stepper::new(
            &gettext("Target Temperature"),
            min,
            max,
            step,
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |value| call(
                    &controls,
                    &entity,
                    "set_temperature",
                    json!({ "temperature": value })
                )
            ),
        );
        controls.add(&stepper.row);
        stepper
    });

    // Range devices reject a low or high value sent alone, so both are sent
    // together.
    let range = entity.supports(TARGET_TEMPERATURE_RANGE).then(|| {
        type Pair = Rc<OnceCell<(glib::WeakRef<adw::SpinRow>, glib::WeakRef<adw::SpinRow>)>>;
        let pair: Pair = Rc::default();
        let commit = |pair: &Pair| {
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                #[strong]
                pair,
                move |_| {
                    let rows = pair
                        .get()
                        .and_then(|(low, high)| Some((low.upgrade()?, high.upgrade()?)));
                    if let Some((low, high)) = rows {
                        call(
                            &controls,
                            &entity,
                            "set_temperature",
                            json!({
                                "target_temp_low": low.value(),
                                "target_temp_high": high.value(),
                            }),
                        );
                    }
                }
            )
        };
        let low = Stepper::new(&gettext("Heat To"), min, max, step, commit(&pair));
        let high = Stepper::new(&gettext("Cool To"), min, max, step, commit(&pair));
        controls.add(&low.row);
        controls.add(&high.row);
        let _ = pair.set((low.row.downgrade(), high.row.downgrade()));
        (low, high)
    });

    let mode = Choice::new(
        &gettext("Mode"),
        glib::clone!(
            #[weak]
            controls,
            #[strong]
            entity,
            move |mode| call(
                &controls,
                &entity,
                "set_hvac_mode",
                json!({ "hvac_mode": mode })
            )
        ),
    );
    controls.add(&mode.row);

    let optional = |feature: u64, title: &str, service: &'static str, key: &'static str| {
        entity.supports(feature).then(|| {
            let choice = Choice::new(
                title,
                glib::clone!(
                    #[weak]
                    controls,
                    #[strong]
                    entity,
                    move |value| call(&controls, &entity, service, json!({ key: value }))
                ),
            );
            controls.add(&choice.row);
            choice
        })
    };
    let preset = optional(
        PRESET_MODE,
        &gettext("Preset"),
        "set_preset_mode",
        "preset_mode",
    );
    let fan = optional(FAN_MODE, &gettext("Fan"), "set_fan_mode", "fan_mode");
    let swing = optional(
        SWING_MODE,
        &gettext("Swing"),
        "set_swing_mode",
        "swing_mode",
    );

    watch(entity, &controls, move |entity| {
        let unit = entity
            .attribute_str("temperature_unit")
            .unwrap_or_else(|| "°".into());
        let reading = entity.attribute_f64("current_temperature");
        current.set_subtitle(&reading.map(|t| format!("{t}{unit}")).unwrap_or_default());
        current.set_visible(reading.is_some());
        if let Some(stepper) = &target {
            stepper.set(entity.attribute_f64("temperature"));
        }
        if let Some((low, high)) = &range {
            low.set(entity.attribute_f64("target_temp_low"));
            high.set(entity.attribute_f64("target_temp_high"));
        }
        mode.set(
            entity.attribute_strings("hvac_modes"),
            Some(&entity.raw_state()),
        );
        for (choice, list, value) in [
            (&preset, "preset_modes", "preset_mode"),
            (&fan, "fan_modes", "fan_mode"),
            (&swing, "swing_modes", "swing_mode"),
        ] {
            if let Some(choice) = choice {
                choice.set(
                    entity.attribute_strings(list),
                    entity.attribute_str(value).as_deref(),
                );
            }
        }
    });
    controls.upcast()
}
