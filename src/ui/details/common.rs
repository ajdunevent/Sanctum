use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::Value;

use crate::ha;
use crate::model::Entity;
use crate::ui::{app, error_text};

/// Server updates are ignored for this long after the user moves a control, so
/// an echo of an older value does not jump the control back.
const USER_HOLD_MICROS: i64 = 2_000_000;
const SLIDER_DEBOUNCE_MS: u64 = 300;

/// Runs `f` now and on every entity update until `widget` is destroyed.
pub fn watch(entity: &Entity, widget: &impl IsA<gtk::Widget>, f: impl Fn(&Entity) + 'static) {
    f(entity);
    let handler = entity.connect_updated(f);
    let entity = entity.downgrade();
    let handler = RefCell::new(Some(handler));
    widget.connect_destroy(move |_| {
        if let (Some(entity), Some(handler)) = (entity.upgrade(), handler.take()) {
            entity.disconnect(handler);
        }
    });
}

pub fn toast(widget: &impl IsA<gtk::Widget>, message: &str) {
    if let Some(overlay) = widget
        .ancestor(adw::ToastOverlay::static_type())
        .and_downcast::<adw::ToastOverlay>()
    {
        let toast = adw::Toast::builder()
            .title(message.replace('\n', ": "))
            .use_markup(false)
            .build();
        overlay.add_toast(toast);
    }
}

pub fn call(widget: &impl IsA<gtk::Widget>, entity: &Entity, service: &str, data: Value) {
    call_domain(widget, &entity.domain(), service, &entity.entity_id(), data);
}

pub fn call_domain(
    widget: &impl IsA<gtk::Widget>,
    domain: &str,
    service: &str,
    entity_id: &str,
    data: Value,
) {
    let client = app().client().clone();
    let widget = widget.clone().upcast::<gtk::Widget>();
    let (domain, service, entity_id) =
        (domain.to_owned(), service.to_owned(), entity_id.to_owned());
    glib::spawn_future_local(async move {
        if let Err(err) =
            ha::services::call_with(&client, &domain, &service, &entity_id, data).await
        {
            toast(&widget, &error_text(&err));
        }
    });
}

pub async fn confirm(
    widget: &impl IsA<gtk::Widget>,
    heading: &str,
    action: &str,
    appearance: adw::ResponseAppearance,
) -> bool {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("confirm", action)]);
    dialog.set_response_appearance("confirm", appearance);
    dialog.choose_future(Some(widget)).await == "confirm"
}

#[derive(Clone, Default)]
struct UserHold(Rc<Cell<i64>>);

impl UserHold {
    fn touch(&self) {
        self.0.set(glib::monotonic_time());
    }

    fn active(&self) -> bool {
        glib::monotonic_time() - self.0.get() < USER_HOLD_MICROS
    }
}

/// A scale that commits after the user pauses, and accepts server values only
/// when the user is not adjusting it.
#[derive(Clone)]
pub struct Slider {
    pub scale: gtk::Scale,
    quiet: Rc<Cell<bool>>,
    hold: UserHold,
}

impl Slider {
    pub fn new(min: f64, max: f64, step: f64, on_commit: impl Fn(f64) + 'static) -> Self {
        let scale =
            gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max.max(min + step), step);
        scale.set_hexpand(true);
        scale.set_draw_value(true);
        scale.set_digits(0);
        scale.set_value_pos(gtk::PositionType::Right);
        let slider = Self {
            scale,
            quiet: Rc::default(),
            hold: UserHold::default(),
        };
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let on_commit = Rc::new(on_commit);
        let (quiet, hold) = (slider.quiet.clone(), slider.hold.clone());
        slider.scale.connect_value_changed(move |scale| {
            if quiet.get() {
                return;
            }
            hold.touch();
            if let Some(source) = pending.take() {
                source.remove();
            }
            let (scale, on_commit, slot, quiet) = (
                scale.clone(),
                on_commit.clone(),
                pending.clone(),
                quiet.clone(),
            );
            let source = glib::timeout_add_local_once(
                std::time::Duration::from_millis(SLIDER_DEBOUNCE_MS),
                move || {
                    slot.take();
                    // Dragging does not snap to the step, and devices such as
                    // fans reject values between their steps.
                    let value = min + ((scale.value() - min) / step).round() * step;
                    quiet.set(true);
                    scale.set_value(value);
                    quiet.set(false);
                    on_commit(value);
                },
            );
            pending.replace(Some(source));
        });
        slider
    }

    pub fn with_unit(self, unit: &'static str) -> Self {
        self.scale
            .set_format_value_func(move |_, value| format!("{value:.0}{unit}"));
        self
    }

    pub fn set(&self, value: f64) {
        if self.hold.active() {
            return;
        }
        self.quiet.set(true);
        self.scale.set_value(value);
        self.quiet.set(false);
    }
}

/// A spin row with the same debounce and hold rules as `Slider`.
#[derive(Clone)]
pub struct Stepper {
    pub row: adw::SpinRow,
    quiet: Rc<Cell<bool>>,
    hold: UserHold,
}

impl Stepper {
    pub fn new(
        title: &str,
        min: f64,
        max: f64,
        step: f64,
        on_commit: impl Fn(f64) + 'static,
    ) -> Self {
        let digits = if step.fract() == 0.0 { 0 } else { 1 };
        let row = adw::SpinRow::with_range(min, max.max(min + step), step);
        row.set_title(title);
        row.set_digits(digits);
        let stepper = Self {
            row,
            quiet: Rc::default(),
            hold: UserHold::default(),
        };
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let on_commit = Rc::new(on_commit);
        let (quiet, hold) = (stepper.quiet.clone(), stepper.hold.clone());
        stepper.row.connect_value_notify(move |row| {
            if quiet.get() {
                return;
            }
            hold.touch();
            if let Some(source) = pending.take() {
                source.remove();
            }
            let (row, on_commit, slot) = (row.clone(), on_commit.clone(), pending.clone());
            let source = glib::timeout_add_local_once(
                std::time::Duration::from_millis(SLIDER_DEBOUNCE_MS * 3),
                move || {
                    slot.take();
                    on_commit(row.value());
                },
            );
            pending.replace(Some(source));
        });
        stepper
    }

    pub fn set(&self, value: Option<f64>) {
        self.row.set_visible(value.is_some());
        let Some(value) = value else { return };
        if self.hold.active() {
            return;
        }
        self.quiet.set(true);
        self.row.set_value(value);
        self.quiet.set(false);
    }
}

/// The slider sits below the title because a row suffix leaves it too little
/// width on phones.
pub fn slider_row(title: &str, slider: &Slider) -> gtk::ListBoxRow {
    let label = gtk::Label::builder().label(title).xalign(0.0).build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    column.append(&label);
    column.append(&slider.scale);
    gtk::ListBoxRow::builder()
        .child(&column)
        .activatable(false)
        .build()
}

/// A switch row whose programmatic updates do not trigger `on_toggle`.
#[derive(Clone)]
pub struct Toggle {
    pub row: adw::SwitchRow,
    quiet: Rc<Cell<bool>>,
}

impl Toggle {
    pub fn new(title: &str, on_toggle: impl Fn(bool) + 'static) -> Self {
        let row = adw::SwitchRow::builder().title(title).build();
        let quiet: Rc<Cell<bool>> = Rc::default();
        let guard = quiet.clone();
        row.connect_active_notify(move |row| {
            if !guard.get() {
                on_toggle(row.is_active());
            }
        });
        Self { row, quiet }
    }

    pub fn set(&self, active: bool) {
        self.quiet.set(true);
        self.row.set_active(active);
        self.quiet.set(false);
    }
}

/// Enum values such as `heat_cool` are humanized; user-defined names such as
/// sources and select options are shown verbatim.
#[derive(Clone)]
pub struct Choice {
    pub row: adw::ComboRow,
    options: Rc<RefCell<Vec<String>>>,
    quiet: Rc<Cell<bool>>,
    humanize: bool,
}

impl Choice {
    pub fn new(title: &str, on_select: impl Fn(String) + 'static) -> Self {
        Self::build(title, true, on_select)
    }

    pub fn verbatim(title: &str, on_select: impl Fn(String) + 'static) -> Self {
        Self::build(title, false, on_select)
    }

    fn build(title: &str, humanize: bool, on_select: impl Fn(String) + 'static) -> Self {
        let row = adw::ComboRow::builder().title(title).build();
        let choice = Self {
            row,
            options: Rc::default(),
            quiet: Rc::default(),
            humanize,
        };
        let (options, quiet) = (choice.options.clone(), choice.quiet.clone());
        choice.row.connect_selected_notify(move |row| {
            if quiet.get() {
                return;
            }
            if let Some(option) = options.borrow().get(row.selected() as usize) {
                on_select(option.clone());
            }
        });
        choice
    }

    pub fn set(&self, options: Vec<String>, current: Option<&str>) {
        self.quiet.set(true);
        if *self.options.borrow() != options {
            let labels: Vec<String> = options
                .iter()
                .map(|option| {
                    if self.humanize {
                        crate::model::present::humanize(option)
                    } else {
                        option.clone()
                    }
                })
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            self.row.set_model(Some(&gtk::StringList::new(&refs)));
            self.options.replace(options);
        }
        let index = current
            .and_then(|current| self.options.borrow().iter().position(|o| o == current))
            .unwrap_or(gtk::INVALID_LIST_POSITION as usize);
        self.row.set_selected(index as u32);
        self.row.set_visible(!self.options.borrow().is_empty());
        self.quiet.set(false);
    }
}

pub fn group(title: &str) -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder().title(title).build()
}

pub fn pill(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .use_underline(true)
        .css_classes(["pill"])
        .hexpand(true)
        .build()
}

pub fn button_box(buttons: &[&gtk::Button]) -> gtk::Box {
    let row = gtk::Box::builder().spacing(12).homogeneous(true).build();
    for button in buttons {
        row.append(*button);
    }
    row
}
