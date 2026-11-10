//! Sanctum's main menu with an icon on every entry. GTK menus show no icons on
//! ordinary entries, so each entry is a custom row of the menu's popover.

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};

/// Material Design Icons path data, as the frontend uses for its entries.
const DASHBOARD: &str = "M19,5V7H15V5H19M9,5V11H5V5H9M19,13V19H15V13H19M9,17V19H5V17H9M21,3H13V9H21V3M11,3H3V13H11V3M21,11H13V21H21V11M11,15H3V21H11V15Z";
const PREFERENCES: &str = "M12,8A4,4 0 0,1 16,12A4,4 0 0,1 12,16A4,4 0 0,1 8,12A4,4 0 0,1 12,8M12,10A2,2 0 0,0 10,12A2,2 0 0,0 12,14A2,2 0 0,0 14,12A2,2 0 0,0 12,10M10,22C9.75,22 9.54,21.82 9.5,21.58L9.13,18.93C8.5,18.68 7.96,18.34 7.44,17.94L4.95,18.95C4.73,19.03 4.46,18.95 4.34,18.73L2.34,15.27C2.21,15.05 2.27,14.78 2.46,14.63L4.57,12.97L4.5,12L4.57,11L2.46,9.37C2.27,9.22 2.21,8.95 2.34,8.73L4.34,5.27C4.46,5.05 4.73,4.96 4.95,5.05L7.44,6.05C7.96,5.66 8.5,5.32 9.13,5.07L9.5,2.42C9.54,2.18 9.75,2 10,2H14C14.25,2 14.46,2.18 14.5,2.42L14.87,5.07C15.5,5.32 16.04,5.66 16.56,6.05L19.05,5.05C19.27,4.96 19.54,5.05 19.66,5.27L21.66,8.73C21.79,8.95 21.73,9.22 21.54,9.37L19.43,11L19.5,12L19.43,13L21.54,14.63C21.73,14.78 21.79,15.05 21.66,15.27L19.66,18.73C19.54,18.95 19.27,19.04 19.05,18.95L16.56,17.95C16.04,18.34 15.5,18.68 14.87,18.93L14.5,21.58C14.46,21.82 14.25,22 14,22H10M11.25,4L10.88,6.61C9.68,6.86 8.62,7.5 7.85,8.39L5.44,7.35L4.69,8.65L6.8,10.2C6.4,11.37 6.4,12.64 6.8,13.8L4.68,15.36L5.43,16.66L7.86,15.62C8.63,16.5 9.68,17.14 10.87,17.38L11.24,20H12.76L13.13,17.39C14.32,17.14 15.37,16.5 16.14,15.62L18.57,16.66L19.32,15.36L17.2,13.81C17.6,12.64 17.6,11.37 17.2,10.2L19.31,8.65L18.56,7.35L16.15,8.39C15.38,7.5 14.32,6.86 13.12,6.62L12.75,4H11.25Z";
const ABOUT: &str = "M11,9H13V7H11M12,20C7.59,20 4,16.41 4,12C4,7.59 7.59,4 12,4C16.41,4 20,7.59 20,12C20,16.41 16.41,20 12,20M12,2A10,10 0 0,0 2,12A10,10 0 0,0 12,22A10,10 0 0,0 22,12A10,10 0 0,0 12,2M11,17H13V11H11V17Z";
const QUIT: &str = "M16.56,5.44L15.11,6.89C16.84,7.94 18,9.83 18,12A6,6 0 0,1 12,18A6,6 0 0,1 6,12C6,9.83 7.16,7.94 8.88,6.88L7.44,5.44C5.36,6.88 4,9.28 4,12A8,8 0 0,0 12,20A8,8 0 0,0 20,12C20,9.28 18.64,6.88 16.56,5.44M13,3H11V13H13";

/// One row of the menu.
pub struct Entry {
    pub label: String,
    /// SVG path data in a 24 × 24 box.
    pub icon: Option<String>,
    /// A themed icon, which takes precedence over `icon`.
    pub icon_name: Option<&'static str>,
    /// A detailed action name, such as "app.quit".
    pub action: String,
    /// An accelerator such as "<Control>k", shown as a label only.
    pub accel: Option<String>,
    /// `Some` for a check entry, with its state.
    pub checked: Option<bool>,
    /// The row hides while its action is disabled.
    pub hide_disabled: bool,
}

/// Sanctum's entries, by the custom ids in the menu model.
pub fn own_entries() -> Vec<(&'static str, Entry)> {
    let entry = |label: String, icon: &str, action: &str| Entry {
        label,
        icon: Some(icon.to_owned()),
        icon_name: None,
        action: action.to_owned(),
        accel: None,
        checked: None,
        hide_disabled: false,
    };
    vec![
        (
            "home",
            Entry {
                hide_disabled: true,
                ..entry(gettext("Default Dashboard"), DASHBOARD, "win.overview")
            },
        ),
        (
            "assist",
            Entry {
                icon: None,
                icon_name: Some("sanctum-assist-symbolic"),
                ..entry(gettext("Assist"), "", "app.assist")
            },
        ),
        (
            "preferences",
            entry(
                gettext("Sanctum Preferences"),
                PREFERENCES,
                "app.preferences",
            ),
        ),
        ("about", entry(gettext("About Sanctum"), ABOUT, "app.about")),
        ("quit", entry(gettext("Quit Sanctum"), QUIT, "app.quit")),
    ]
}

/// A symbolic icon for SVG path data. GTK recolors an SVG file named
/// "-symbolic.svg" like a themed icon, so the shape is written to the cache
/// once. Path data with anything but path characters is refused, since it
/// ends up in a file GTK parses.
pub fn path_icon(path: &str) -> Option<gio::Icon> {
    if path.is_empty()
        || !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ,.-".contains(c))
    {
        return None;
    }
    let dir = glib::user_cache_dir().join("sanctum").join("icons");
    let hash = glib::compute_checksum_for_string(glib::ChecksumType::Sha1, path)?;
    let file = dir.join(format!("{hash}-symbolic.svg"));
    if !file.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" \
             viewBox=\"0 0 24 24\"><path fill=\"#000\" d=\"{path}\"/></svg>"
        );
        std::fs::write(&file, svg).ok()?;
    }
    Some(gio::FileIcon::new(&gio::File::for_path(file)).upcast())
}

fn accel_label(accel: &str) -> Option<String> {
    let (key, modifiers) = gtk::accelerator_parse(accel)?;
    Some(gtk::accelerator_get_label(key, modifiers).to_string())
}

/// A row for `entry` that closes `popover` when chosen. Without
/// `entry.accel`, the row shows the application's first accelerator for its
/// action.
pub fn row(entry: &Entry, popover: &gtk::PopoverMenu) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let image = gtk::Image::builder()
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    if let Some(name) = entry.icon_name {
        image.set_icon_name(Some(name));
    } else if let Some(icon) = entry.icon.as_deref().and_then(path_icon) {
        image.set_from_gicon(&icon);
    }
    content.append(&image);
    content.append(
        &gtk::Label::builder()
            .label(&entry.label)
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    let accel = entry.accel.clone().or_else(|| {
        gio::Application::default()
            .and_downcast::<gtk::Application>()
            .and_then(|app| app.accels_for_action(&entry.action).first().cloned())
            .map(|accel| accel.to_string())
    });
    if let Some(label) = accel.as_deref().and_then(accel_label) {
        content.append(
            &gtk::Label::builder()
                .label(label)
                .css_classes(["dim-label"])
                .build(),
        );
    }
    if let Some(checked) = entry.checked {
        let check = gtk::Image::from_icon_name("object-select-symbolic");
        check.set_opacity(if checked { 1.0 } else { 0.0 });
        content.append(&check);
    }
    let button = gtk::Button::builder()
        .child(&content)
        .action_name(&entry.action)
        .css_classes(["flat", "sanctum-menu-row"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&entry.label)]);
    button.connect_clicked(glib::clone!(
        #[weak]
        popover,
        move |_| popover.popdown()
    ));
    if entry.hide_disabled {
        button
            .bind_property("sensitive", &button, "visible")
            .sync_create()
            .build();
    }
    button
}

fn descendants(widget: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    let mut child = widget.first_child();
    while let Some(current) = child {
        out.push(current.clone());
        descendants(&current, out);
        child = current.next_sibling();
    }
}

/// The popover's arrow-key navigation stops at custom rows, so Up and Down move
/// between the rows here. The zoom row counts as one stop.
pub fn add_arrow_navigation(popover: &gtk::PopoverMenu) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        popover,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            let step: isize = match key {
                gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => 1,
                gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => -1,
                _ => return glib::Propagation::Proceed,
            };
            let mut all = Vec::new();
            descendants(popover.upcast_ref(), &mut all);
            let mut stops: Vec<(f32, gtk::Widget)> = all
                .into_iter()
                .filter(|widget| {
                    (widget.has_css_class("sanctum-menu-row")
                        || widget.has_css_class("sanctum-zoom-stop"))
                        && widget.is_drawable()
                        && widget.is_sensitive()
                })
                .filter_map(|widget| {
                    let y = widget
                        .compute_point(&popover, &gtk::graphene::Point::zero())?
                        .y();
                    Some((y, widget))
                })
                .collect();
            stops.sort_by(|a, b| a.0.total_cmp(&b.0));
            let focused = |widget: &gtk::Widget| {
                if widget.has_css_class("sanctum-zoom-stop") {
                    let mut buttons = Vec::new();
                    if let Some(zoom) = widget.parent() {
                        descendants(&zoom, &mut buttons);
                    }
                    buttons.iter().any(WidgetExt::has_focus)
                } else {
                    widget.has_focus()
                }
            };
            let current = stops.iter().position(|(_, stop)| focused(stop));
            let next = match current {
                Some(index) => index as isize + step,
                None if step > 0 => 0,
                None => stops.len() as isize - 1,
            };
            if let Some((_, stop)) = usize::try_from(next).ok().and_then(|next| stops.get(next)) {
                if let Some(window) = popover.root().and_downcast::<gtk::Window>() {
                    window.set_focus_visible(true);
                }
                stop.grab_focus();
            }
            glib::Propagation::Stop
        }
    ));
    popover.add_controller(keys);
}
