use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use serde_json::Map;

use super::common::watch;
use crate::ha::history::{self, Point};
use crate::model::Entity;
use crate::model::present;
use crate::ui::app;

const HOURS: i64 = 24;
const GRAPH_HEIGHT: i32 = 96;
const HEAT: &str = "#e66100";
const COOL: &str = "#2190a4";

fn now() -> f64 {
    glib::real_time() as f64 / 1_000_000.0
}

struct Series {
    label: String,
    /// `None` uses the accent color.
    color: Option<&'static str>,
    /// Setpoints hold their value until changed; readings are joined directly.
    step: bool,
    /// `None` reads the entity state.
    attribute: Option<&'static str>,
    /// NaN marks a gap.
    values: Vec<(f64, f64)>,
}

enum Chart {
    Lines { series: Vec<Series>, unit: String },
    Timeline(Vec<(f64, String)>),
}

/// Series drawn from attributes, for domains whose state is a mode rather than
/// the reading people want to see over time.
fn attribute_series(entity: &Entity) -> Option<(Vec<Series>, String)> {
    let series = |attribute, label: String, color, step| Series {
        label,
        color,
        step,
        attribute: Some(attribute),
        values: Vec::new(),
    };
    let temperature_unit = entity
        .attribute_str("temperature_unit")
        .unwrap_or_else(|| "°".into());
    match entity.domain().as_str() {
        "climate" => Some((
            vec![
                series("current_temperature", gettext("Current"), None, false),
                series("temperature", gettext("Target"), Some(HEAT), true),
                series("target_temp_low", gettext("Heat To"), Some(HEAT), true),
                series("target_temp_high", gettext("Cool To"), Some(COOL), true),
            ],
            temperature_unit,
        )),
        "water_heater" => Some((
            vec![
                series("current_temperature", gettext("Current"), None, false),
                series("temperature", gettext("Target"), Some(HEAT), true),
            ],
            temperature_unit,
        )),
        "humidifier" => Some((
            vec![
                series("current_humidity", gettext("Current"), None, false),
                series("humidity", gettext("Target"), Some(HEAT), true),
            ],
            "%".into(),
        )),
        _ => None,
    }
}

/// Text states are drawn as a timeline; mostly numeric ones as a line.
fn is_numeric(points: &[Point]) -> bool {
    let numeric = history::numeric(points).len();
    numeric >= 2 && numeric * 2 >= points.len()
}

fn chart(entity: &Entity, points: &[Point]) -> Chart {
    if let Some((mut series, unit)) = attribute_series(entity) {
        for line in &mut series {
            line.values = history::attribute(points, line.attribute.unwrap_or_default());
        }
        series.retain(|line| line.values.iter().any(|(_, v)| v.is_finite()));
        if !series.is_empty() {
            return Chart::Lines { series, unit };
        }
    } else if is_numeric(points) {
        let series = vec![Series {
            label: String::new(),
            color: None,
            step: false,
            attribute: None,
            values: history::numeric(points),
        }];
        let unit = entity
            .attribute_str("unit_of_measurement")
            .unwrap_or_default();
        return Chart::Lines { series, unit };
    }
    Chart::Timeline(
        points
            .iter()
            .map(|point| (point.time, point.state.clone()))
            .collect(),
    )
}

impl Chart {
    fn is_empty(&self) -> bool {
        match self {
            Self::Lines { series, .. } => series.is_empty(),
            Self::Timeline(states) => states.is_empty(),
        }
    }

    /// Appends the entity's current values when they differ from the last ones.
    fn extend(&mut self, entity: &Entity) {
        let time = now();
        match self {
            Self::Lines { series, .. } => {
                for line in series {
                    let value = match line.attribute {
                        Some(key) => entity.attribute_f64(key),
                        None => entity.raw_state().parse().ok(),
                    }
                    .unwrap_or(f64::NAN);
                    let repeat = line.values.last().is_some_and(|(_, last)| {
                        *last == value || (last.is_nan() && value.is_nan())
                    });
                    if !repeat {
                        line.values.push((time, value));
                    }
                }
            }
            Self::Timeline(states) => {
                let state = entity.raw_state();
                if states.last().is_none_or(|(_, last)| *last != state) {
                    let start = entity
                        .last_changed()
                        .max(states.last().map_or(0.0, |(t, _)| *t));
                    states.push((start, state));
                }
            }
        }
    }
}

fn trim(value: f64) -> String {
    let text = format!("{value:.1}");
    text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
}

fn with_unit(value: f64, unit: &str) -> String {
    if !value.is_finite() {
        return "–".into();
    }
    match unit {
        "" => trim(value),
        "%" | "°" => format!("{}{unit}", trim(value)),
        _ => format!("{} {unit}", trim(value)),
    }
}

fn bounds(series: &[Series]) -> (f64, f64) {
    series
        .iter()
        .flat_map(|line| &line.values)
        .filter(|(_, v)| v.is_finite())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (_, v)| {
            (lo.min(*v), hi.max(*v))
        })
}

/// The last value recorded at or before `time`.
fn value_at<T: Clone>(values: &[(f64, T)], time: f64) -> Option<T> {
    values
        .iter()
        .take_while(|(t, _)| *t <= time)
        .last()
        .map(|(_, v)| v.clone())
}

/// A clock time in the locale's 12 or 24 hour style, with the weekday when it
/// is not today.
fn clock(time: f64) -> String {
    let Ok(when) = glib::DateTime::from_unix_local(time as i64) else {
        return String::new();
    };
    let twelve_hour = when.format("%p").is_ok_and(|p| !p.is_empty());
    let mut format = if twelve_hour { "%-I:%M %p" } else { "%H:%M" }.to_owned();
    let today = glib::DateTime::now_local().is_ok_and(|now| now.ymd() == when.ymd());
    if !today {
        format.insert_str(0, "%a ");
    }
    when.format(&format)
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn hex(color: &gtk::gdk::RGBA) -> String {
    let channel = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.red()),
        channel(color.green()),
        channel(color.blue())
    )
}

fn accent() -> gtk::gdk::RGBA {
    adw::StyleManager::default().accent_color_rgba()
}

fn color_of(line: &Series) -> gtk::gdk::RGBA {
    line.color
        .and_then(|c| gtk::gdk::RGBA::parse(c).ok())
        .unwrap_or_else(accent)
}

/// The time and values at `time`, or the latest values when `None`.
fn readout(chart: &Chart, time: Option<f64>, domain: &str) -> String {
    let at = time.unwrap_or(f64::INFINITY);
    let when = time.map_or_else(|| gettext("Now"), clock);
    let mut text = format!("<b>{}</b>", glib::markup_escape_text(&when));
    match chart {
        Chart::Lines { series, unit } => {
            for line in series {
                let value = value_at(&line.values, at).unwrap_or(f64::NAN);
                text.push_str("   ");
                if !line.label.is_empty() {
                    text.push_str(&format!(
                        "<span foreground=\"{}\">●</span> {} ",
                        hex(&color_of(line)),
                        glib::markup_escape_text(&line.label)
                    ));
                }
                text.push_str(&glib::markup_escape_text(&with_unit(value, unit)));
            }
        }
        Chart::Timeline(states) => {
            if let Some(state) = value_at(states, at) {
                let label = present::state_text(domain, &state, &Map::new());
                text.push_str("   ");
                text.push_str(&glib::markup_escape_text(&label));
            }
        }
    }
    text
}

/// A 24 hour graph that keeps extending with live updates while it is shown.
/// Hovering or dragging across it shows the values at that moment.
pub fn build(entity: &Entity) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let title = gtk::Label::builder()
        .label(gettext("Last 24 Hours"))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["heading"])
        .build();
    let range = gtk::Label::builder()
        .css_classes(["caption", "dim-label", "numeric"])
        .build();
    header.append(&title);
    header.append(&range);

    let values = gtk::Label::builder()
        .xalign(0.0)
        .use_markup(true)
        .wrap(true)
        .css_classes(["caption", "numeric"])
        .build();

    let area = gtk::DrawingArea::builder()
        .content_height(GRAPH_HEIGHT)
        .hexpand(true)
        .css_classes(["history-graph"])
        .build();
    let status = gtk::Label::builder()
        .label(gettext("Loading…"))
        .height_request(GRAPH_HEIGHT)
        .css_classes(["dim-label"])
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&status, Some("status"));
    stack.add_named(&area, Some("graph"));

    let axis = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let start = gtk::Label::builder()
        .label(gettext("24 h ago"))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["caption", "dim-label"])
        .build();
    let end = gtk::Label::builder()
        .label(gettext("Now"))
        .xalign(1.0)
        .css_classes(["caption", "dim-label"])
        .build();
    axis.append(&start);
    axis.append(&end);

    column.append(&header);
    column.append(&values);
    column.append(&stack);
    column.append(&axis);
    values.set_visible(false);
    axis.set_visible(false);

    let state: Rc<RefCell<Option<Chart>>> = Rc::default();
    // Fraction of the width under the pointer or finger.
    let hover: Rc<Cell<Option<f64>>> = Rc::default();
    let domain = entity.domain();

    area.set_draw_func(glib::clone!(
        #[strong]
        state,
        #[strong]
        hover,
        #[strong]
        domain,
        move |area, cr, width, height| {
            if let Some(chart) = state.borrow().as_ref() {
                draw(
                    area,
                    cr,
                    width as f64,
                    height as f64,
                    chart,
                    &domain,
                    hover.get(),
                );
            }
        }
    ));

    let refresh = Rc::new(glib::clone!(
        #[strong]
        state,
        #[strong]
        hover,
        #[strong]
        domain,
        #[weak]
        area,
        #[weak]
        range,
        #[weak]
        values,
        #[weak]
        stack,
        #[weak]
        axis,
        move || {
            let chart = state.borrow();
            let Some(chart) = chart.as_ref() else {
                return;
            };
            let empty = chart.is_empty();
            stack.set_visible_child_name(if empty { "status" } else { "graph" });
            axis.set_visible(!empty);
            values.set_visible(!empty);
            if empty {
                return;
            }
            match chart {
                Chart::Lines { series, unit } if series.len() == 1 => {
                    let (min, max) = bounds(series);
                    range.set_label(&format!(
                        "{} – {}",
                        with_unit(min, unit),
                        with_unit(max, unit)
                    ));
                }
                _ => range.set_label(""),
            }
            let end = now();
            let time = hover
                .get()
                .map(|fraction| end - (1.0 - fraction) * HOURS as f64 * 3600.0);
            values.set_label(&readout(chart, time, &domain));
            area.queue_draw();
        }
    ));

    let set_hover = glib::clone!(
        #[strong]
        hover,
        #[strong]
        refresh,
        #[weak]
        area,
        move |x: Option<f64>| {
            let width = area.width().max(1) as f64;
            hover.set(x.map(|x| (x / width).clamp(0.0, 1.0)));
            refresh();
        }
    );
    let set_hover = Rc::new(set_hover);

    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion(glib::clone!(
        #[strong]
        set_hover,
        move |_, x, _| set_hover(Some(x))
    ));
    motion.connect_leave(glib::clone!(
        #[strong]
        set_hover,
        move |_| set_hover(None)
    ));
    area.add_controller(motion);

    // Touch scrubbing yields to vertical scrolling when the finger moves
    // mostly up or down.
    let drag = gtk::GestureDrag::builder().touch_only(true).build();
    let origin: Rc<Cell<f64>> = Rc::default();
    drag.connect_drag_begin(glib::clone!(
        #[strong]
        set_hover,
        #[strong]
        origin,
        move |_, x, _| {
            origin.set(x);
            set_hover(Some(x));
        }
    ));
    drag.connect_drag_update(glib::clone!(
        #[strong]
        set_hover,
        #[strong]
        origin,
        move |gesture, dx, dy| {
            if dy.abs() > dx.abs() && dy.abs() > 8.0 {
                gesture.set_state(gtk::EventSequenceState::Denied);
                set_hover(None);
                return;
            }
            if dx.abs() > 8.0 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
            set_hover(Some(origin.get() + dx));
        }
    ));
    drag.connect_drag_end(glib::clone!(
        #[strong]
        set_hover,
        move |_, _, _| set_hover(None)
    ));
    area.add_controller(drag);

    let client = app().client().clone();
    let with_attributes = attribute_series(entity).is_some();
    glib::spawn_future_local(glib::clone!(
        #[strong]
        state,
        #[strong]
        refresh,
        #[weak]
        status,
        #[weak]
        entity,
        async move {
            let entity_id = entity.entity_id();
            match history::fetch(&client, &entity_id, HOURS, with_attributes).await {
                Ok(points) => {
                    let mut chart = chart(&entity, &points);
                    chart.extend(&entity);
                    state.replace(Some(chart));
                    status.set_label(&gettext("No history"));
                    refresh();
                }
                Err(err) => {
                    log::debug!("history for {entity_id}: {err}");
                    status.set_label(&gettext("History is unavailable"));
                }
            }
        }
    ));

    watch(entity, &column, move |entity| {
        if let Some(chart) = state.borrow_mut().as_mut() {
            chart.extend(entity);
        } else {
            return;
        }
        refresh();
    });
    column.upcast()
}

fn draw(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: f64,
    height: f64,
    chart: &Chart,
    domain: &str,
    hover: Option<f64>,
) {
    let foreground = area.color();
    let end = now();
    let start = end - HOURS as f64 * 3600.0;
    let x = |time: f64| ((time.clamp(start, end) - start) / (end - start)) * width;

    rounded(cr, width, height, 8.0);
    cr.clip();
    set(cr, &foreground, 0.06);
    let _ = cr.paint();

    match chart {
        Chart::Lines { series, .. } => {
            let (mut min, mut max) = bounds(series);
            if (max - min).abs() < f64::EPSILON {
                min -= 1.0;
                max += 1.0;
            }
            let pad = (max - min) * 0.12;
            let y = |value: f64| height - ((value - min + pad) / (max - min + pad * 2.0)) * height;

            for (index, line) in series.iter().enumerate() {
                let color = color_of(line);
                let segments = segments(line, end);
                // Only the reading is filled; fills under setpoint lines
                // would hide it.
                if index == 0 && !line.step {
                    for segment in &segments {
                        trace(cr, segment, line.step, &x, &y);
                        let (first, last) = (segment[0].0, segment[segment.len() - 1].0);
                        cr.line_to(x(last), height);
                        cr.line_to(x(first), height);
                        cr.close_path();
                        set(cr, &color, 0.18);
                        let _ = cr.fill();
                    }
                }
                for segment in &segments {
                    trace(cr, segment, line.step, &x, &y);
                }
                set(cr, &color, 1.0);
                cr.set_line_width(2.0);
                cr.set_line_join(gtk::cairo::LineJoin::Round);
                if line.step {
                    cr.set_dash(&[5.0, 3.0], 0.0);
                }
                let _ = cr.stroke();
                cr.set_dash(&[], 0.0);
            }

            if let Some(fraction) = hover {
                let px = fraction * width;
                cursor(cr, &foreground, px, height);
                let time = start + fraction * (end - start);
                for line in series {
                    if let Some(value) = value_at(&line.values, time).filter(|v| v.is_finite()) {
                        cr.arc(px, y(value), 4.0, 0.0, std::f64::consts::TAU);
                        set(cr, &color_of(line), 1.0);
                        let _ = cr.fill();
                    }
                }
            }
        }
        Chart::Timeline(states) => {
            let accent = accent();
            // Adjacent runs in the same style are merged and snapped to whole
            // pixels, since antialiased edges between them show as seams.
            let style = |state: &str| {
                if !present::is_available(state) {
                    0
                } else if present::is_active(domain, state) {
                    2
                } else {
                    1
                }
            };
            let mut runs: Vec<(f64, f64, u8)> = Vec::new();
            for (index, (time, state)) in states.iter().enumerate() {
                let next = states.get(index + 1).map_or(end, |(next, _)| *next);
                let (left, right, kind) = (x(*time).round(), x(next).round(), style(state));
                match runs.last_mut() {
                    Some(run) if run.2 == kind => run.1 = right,
                    _ => runs.push((left, right, kind)),
                }
            }
            for (left, right, kind) in runs {
                if right <= left || kind == 0 {
                    continue;
                }
                if kind == 2 {
                    set(cr, &accent, 1.0);
                } else {
                    set(cr, &foreground, 0.18);
                }
                cr.rectangle(left, 0.0, right - left, height);
                let _ = cr.fill();
            }
            if let Some(fraction) = hover {
                cursor(cr, &foreground, fraction * width, height);
            }
        }
    }
}

/// Runs of finite values, each extended to `end` or to the gap that ends it.
fn segments(line: &Series, end: f64) -> Vec<Vec<(f64, f64)>> {
    let mut segments: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();
    for &(time, value) in &line.values {
        if value.is_finite() {
            current.push((time, value));
        } else if let Some(&(_, last)) = current.last() {
            current.push((time, last));
            segments.push(std::mem::take(&mut current));
        }
    }
    if let Some(&(_, last)) = current.last() {
        current.push((end, last));
        segments.push(current);
    }
    segments
}

fn trace(
    cr: &gtk::cairo::Context,
    segment: &[(f64, f64)],
    step: bool,
    x: &impl Fn(f64) -> f64,
    y: &impl Fn(f64) -> f64,
) {
    let (t0, v0) = segment[0];
    cr.move_to(x(t0), y(v0));
    let mut previous = v0;
    for &(time, value) in &segment[1..] {
        if step {
            cr.line_to(x(time), y(previous));
        }
        cr.line_to(x(time), y(value));
        previous = value;
    }
}

fn cursor(cr: &gtk::cairo::Context, foreground: &gtk::gdk::RGBA, px: f64, height: f64) {
    let px = px.round() + 0.5;
    cr.move_to(px, 0.0);
    cr.line_to(px, height);
    set(cr, foreground, 0.5);
    cr.set_line_width(1.0);
    let _ = cr.stroke();
}

fn set(cr: &gtk::cairo::Context, color: &gtk::gdk::RGBA, alpha: f32) {
    cr.set_source_rgba(
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
        (color.alpha() * alpha) as f64,
    );
}

fn rounded(cr: &gtk::cairo::Context, width: f64, height: f64, radius: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(width - radius, radius, radius, -FRAC_PI_2, 0.0);
    cr.arc(width - radius, height - radius, radius, 0.0, FRAC_PI_2);
    cr.arc(radius, height - radius, radius, FRAC_PI_2, PI);
    cr.arc(radius, radius, radius, PI, PI * 1.5);
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_break_at_gaps_and_extend() {
        let line = Series {
            label: String::new(),
            color: None,
            step: true,
            attribute: None,
            values: vec![(0.0, 20.0), (10.0, 21.0), (20.0, f64::NAN), (30.0, 22.0)],
        };
        let segments = segments(&line, 40.0);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0], vec![(0.0, 20.0), (10.0, 21.0), (20.0, 21.0)]);
        assert_eq!(segments[1], vec![(30.0, 22.0), (40.0, 22.0)]);
    }

    #[test]
    fn value_lookup() {
        let values = vec![(0.0, 1.0), (10.0, 2.0)];
        assert_eq!(value_at(&values, -1.0), None);
        assert_eq!(value_at(&values, 5.0), Some(1.0));
        assert_eq!(value_at(&values, f64::INFINITY), Some(2.0));
    }
}
