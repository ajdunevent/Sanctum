use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::json;

use super::common::{Choice, Slider, Toggle, call, group, slider_row, watch};
use crate::model::Entity;

const PAUSE: u64 = 1;
const VOLUME_SET: u64 = 4;
const VOLUME_MUTE: u64 = 8;
const PREVIOUS_TRACK: u64 = 16;
const NEXT_TRACK: u64 = 32;
const TURN_ON: u64 = 128;
const TURN_OFF: u64 = 256;
const SELECT_SOURCE: u64 = 2048;
const PLAY: u64 = 16384;

fn transport(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .css_classes(["circular", "flat", "media-transport"])
        .valign(gtk::Align::Center)
        .build();
    crate::ui::label_by_tooltip(&button);
    button
}

pub fn build(entity: &Entity) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 18);

    let title = gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["title-3"])
        .build();
    let artist = gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build();
    column.append(&title);
    column.append(&artist);

    let bar = gtk::CenterBox::new();
    let middle = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let previous = transport("media-skip-backward-symbolic", &gettext("Previous"));
    let play = transport("media-playback-start-symbolic", &gettext("Play"));
    play.remove_css_class("flat");
    play.add_css_class("suggested-action");
    play.add_css_class("media-play");
    let next = transport("media-skip-forward-symbolic", &gettext("Next"));
    previous.set_visible(entity.supports(PREVIOUS_TRACK));
    play.set_visible(entity.supports(PAUSE | PLAY));
    next.set_visible(entity.supports(NEXT_TRACK));
    for (button, service) in [
        (&previous, "media_previous_track"),
        (&play, "media_play_pause"),
        (&next, "media_next_track"),
    ] {
        button.connect_clicked(glib::clone!(
            #[strong]
            entity,
            move |button| call(button, &entity, service, json!({}))
        ));
        middle.append(button);
    }
    bar.set_center_widget(Some(&middle));
    let power = transport("system-shutdown-symbolic", &gettext("Power"));
    power.set_visible(entity.supports(TURN_ON | TURN_OFF));
    power.connect_clicked(glib::clone!(
        #[strong]
        entity,
        move |button| {
            let service = if entity.raw_state() == "off" {
                "turn_on"
            } else {
                "turn_off"
            };
            call(button, &entity, service, json!({}));
        }
    ));
    bar.set_end_widget(Some(&power));
    column.append(&bar);

    let controls = group("");
    let volume = entity.supports(VOLUME_SET).then(|| {
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
                    "volume_set",
                    json!({ "volume_level": value.round() / 100.0 })
                )
            ),
        )
        .with_unit("%");
        controls.add(&slider_row(&gettext("Volume"), &slider));
        slider
    });
    let mute = entity.supports(VOLUME_MUTE).then(|| {
        let toggle = Toggle::new(
            &gettext("Mute"),
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |muted| call(
                    &controls,
                    &entity,
                    "volume_mute",
                    json!({ "is_volume_muted": muted })
                )
            ),
        );
        controls.add(&toggle.row);
        toggle
    });
    let source = entity.supports(SELECT_SOURCE).then(|| {
        let choice = Choice::verbatim(
            &gettext("Source"),
            glib::clone!(
                #[weak]
                controls,
                #[strong]
                entity,
                move |source| call(
                    &controls,
                    &entity,
                    "select_source",
                    json!({ "source": source })
                )
            ),
        );
        controls.add(&choice.row);
        choice
    });
    if volume.is_some() || mute.is_some() || source.is_some() {
        column.append(&controls);
    }

    watch(entity, &column, move |entity| {
        let state = entity.raw_state();
        let media_title = entity.attribute_str("media_title").unwrap_or_default();
        let media_artist = entity
            .attribute_str("media_artist")
            .or_else(|| entity.attribute_str("media_album_name"))
            .unwrap_or_default();
        title.set_label(&media_title);
        title.set_visible(!media_title.is_empty());
        artist.set_label(&media_artist);
        artist.set_visible(!media_artist.is_empty());

        let playing = state == "playing";
        play.set_icon_name(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        play.set_tooltip_text(Some(&if playing {
            gettext("Pause")
        } else {
            gettext("Play")
        }));
        let on = state != "off" && state != "standby";
        for button in [&previous, &play, &next] {
            button.set_sensitive(on);
        }

        if let (Some(slider), Some(level)) = (&volume, entity.attribute_f64("volume_level")) {
            slider.set((level * 100.0).round());
        }
        if let Some(toggle) = &mute {
            toggle.set(
                entity
                    .attribute("is_volume_muted")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            );
        }
        if let Some(choice) = &source {
            choice.set(
                entity.attribute_strings("source_list"),
                entity.attribute_str("source").as_deref(),
            );
        }
    });
    column.upcast()
}
