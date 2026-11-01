use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;

use super::common::{call, confirm, group, pill, watch};
use super::lock::{code_row, take_code};
use crate::model::Entity;

const ARM_HOME: u64 = 1;
const ARM_AWAY: u64 = 2;
const ARM_NIGHT: u64 = 4;
const ARM_CUSTOM_BYPASS: u64 = 16;
const ARM_VACATION: u64 = 32;

pub fn build(entity: &Entity) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 18);
    let code = code_row(entity);
    if let Some(row) = &code {
        let codes = group("");
        codes.add(row);
        column.append(&codes);
    }

    let disarm = pill(&gettext("_Disarm"));
    disarm.add_css_class("destructive-action");
    disarm.connect_clicked(glib::clone!(
        #[strong]
        entity,
        #[strong]
        code,
        move |button| {
            let (button, entity, code) = (button.clone(), entity.clone(), code.clone());
            let heading = gettext("Disarm %s?").replacen("%s", &entity.name(), 1);
            glib::spawn_future_local(async move {
                if confirm(
                    &button,
                    &heading,
                    &gettext("_Disarm"),
                    adw::ResponseAppearance::Destructive,
                )
                .await
                {
                    call(&button, &entity, "alarm_disarm", take_code(code.as_ref()));
                }
            });
        }
    ));

    let arm = gtk::Grid::builder()
        .column_homogeneous(true)
        .row_spacing(12)
        .column_spacing(12)
        .build();
    let mut arm_buttons = Vec::new();
    for (feature, label, service, state) in [
        (
            ARM_HOME,
            gettext("Arm _Home"),
            "alarm_arm_home",
            "armed_home",
        ),
        (
            ARM_AWAY,
            gettext("Arm _Away"),
            "alarm_arm_away",
            "armed_away",
        ),
        (
            ARM_NIGHT,
            gettext("Arm _Night"),
            "alarm_arm_night",
            "armed_night",
        ),
        (
            ARM_VACATION,
            gettext("Arm _Vacation"),
            "alarm_arm_vacation",
            "armed_vacation",
        ),
        (
            ARM_CUSTOM_BYPASS,
            gettext("Arm _Custom"),
            "alarm_arm_custom_bypass",
            "armed_custom_bypass",
        ),
    ] {
        if !entity.supports(feature) {
            continue;
        }
        let button = pill(&label);
        button.connect_clicked(glib::clone!(
            #[strong]
            entity,
            #[strong]
            code,
            move |button| call(button, &entity, service, take_code(code.as_ref()))
        ));
        let index = arm_buttons.len() as i32;
        arm.attach(&button, index % 2, index / 2, 1, 1);
        arm_buttons.push((button, state));
    }

    column.append(&disarm);
    column.append(&arm);

    watch(entity, &column, move |entity| {
        let state = entity.raw_state();
        disarm.set_sensitive(state != "disarmed");
        for (button, armed_state) in &arm_buttons {
            button.set_sensitive(state != *armed_state);
        }
    });
    column.upcast()
}
