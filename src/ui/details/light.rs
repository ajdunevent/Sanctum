use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::json;

use super::common::{Slider, Toggle, call, group, slider_row, watch};
use crate::model::Entity;

/// Hue presets shown as swatches; saturation is always 100.
const SWATCHES: [(&str, f64); 8] = [
    ("red", 0.0),
    ("orange", 30.0),
    ("yellow", 55.0),
    ("green", 120.0),
    ("teal", 180.0),
    ("blue", 230.0),
    ("purple", 275.0),
    ("pink", 320.0),
];

fn modes(entity: &Entity) -> Vec<String> {
    entity.attribute_strings("supported_color_modes")
}

pub fn build(entity: &Entity) -> gtk::Widget {
    let modes = modes(entity);
    let dims = modes
        .iter()
        .any(|mode| mode != "onoff" && mode != "unknown");
    let has_temperature = modes.iter().any(|mode| mode == "color_temp");
    let has_color = modes
        .iter()
        .any(|mode| matches!(mode.as_str(), "hs" | "xy" | "rgb" | "rgbw" | "rgbww"));

    let column = gtk::Box::new(gtk::Orientation::Vertical, 18);
    let controls = group("");
    column.append(&controls);

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

    let brightness = dims.then(|| {
        let slider = Slider::new(
            1.0,
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
                    "turn_on",
                    json!({ "brightness_pct": value.round() })
                )
            ),
        )
        .with_unit("%");
        controls.add(&slider_row(&gettext("Brightness"), &slider));
        slider
    });

    let temperature = has_temperature.then(|| {
        let min = entity
            .attribute_f64("min_color_temp_kelvin")
            .unwrap_or(2000.0);
        let max = entity
            .attribute_f64("max_color_temp_kelvin")
            .unwrap_or(6500.0);
        let slider = Slider::new(
            min,
            max,
            50.0,
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |value| call(
                    &controls,
                    &entity,
                    "turn_on",
                    json!({ "color_temp_kelvin": value.round() })
                )
            ),
        )
        .with_unit(" K");
        slider.scale.add_css_class("color-temperature");
        controls.add(&slider_row(&gettext("Color Temperature"), &slider));
        slider
    });

    if has_color {
        let swatches = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(4)
            .max_children_per_line(8)
            .row_spacing(8)
            .column_spacing(6)
            .build();
        for (name, hue) in SWATCHES {
            let swatch = gtk::Button::builder()
                .css_classes(["circular", "color-swatch", &format!("swatch-{name}")])
                .tooltip_text(crate::model::present::humanize(name))
                .halign(gtk::Align::Center)
                .build();
            swatch.connect_clicked(glib::clone!(
                #[strong]
                entity,
                move |button| call(
                    button,
                    &entity,
                    "turn_on",
                    json!({ "hs_color": [hue, 100] })
                )
            ));
            swatches.append(&swatch);
        }
        let color = group(&gettext("Color"));
        color.add(&swatches);
        column.append(&color);
    }

    watch(entity, &column, move |entity| {
        let on = entity.raw_state() == "on";
        power.set(on);
        if let (Some(slider), Some(value)) = (&brightness, entity.attribute_f64("brightness")) {
            slider.set((value / 255.0 * 100.0).round());
        }
        if let (Some(slider), Some(value)) =
            (&temperature, entity.attribute_f64("color_temp_kelvin"))
        {
            slider.set(value);
        }
    });
    column.upcast()
}
