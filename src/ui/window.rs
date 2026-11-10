use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use webkit::prelude::*;

use super::Onboarding;
use super::dashboard::{DashboardView, HeaderColors, HeaderStyle, PageMenuItem, Toolbar};
use super::menu;
use crate::application::Application;
use crate::ha::ConnectionState;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/net/dunevent/Sanctum/ui/window.ui")]
    pub struct Window {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub onboarding: TemplateChild<Onboarding>,
        #[template_child]
        pub disconnected_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub certificate_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub retry_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub web_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub web_holder: TemplateChild<adw::Bin>,
        #[template_child]
        pub main_view: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub registration_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub start_controls_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub start_controls: TemplateChild<gtk::WindowControls>,
        #[template_child]
        pub end_controls_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub end_controls: TemplateChild<gtk::WindowControls>,
        #[template_child]
        pub section_menu: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub header_menu: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub start_menu: TemplateChild<gtk::MenuButton>,
        pub dashboard: OnceCell<Rc<DashboardView>>,
        pub controls_style: gtk::CssProvider,
        /// Handlers on the client and settings, which outlive the window.
        pub handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
        pub toolbar: Cell<Toolbar>,
        /// Actions of the toolbar's overflow menu entries, under "page".
        pub page_actions: gio::SimpleActionGroup,
        /// True while those entries are a section of Sanctum's menu.
        pub page_section: Cell<bool>,
        /// The rows standing for those entries in each menu popover.
        pub page_rows: RefCell<Vec<(gtk::PopoverMenu, gtk::Widget)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "SanctumWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            Onboarding::ensure_type();
            klass.bind_template();
            klass.bind_template_instance_callbacks();
            for (name, step) in [
                ("win.zoom-out", -1),
                ("win.zoom-reset", 0),
                ("win.zoom-in", 1),
            ] {
                klass.install_action(name, None, move |window, _, _| {
                    if let Some(app) = window.app() {
                        let settings = app.settings();
                        let _ = settings
                            .set_double("web-zoom", next_zoom(settings.double("web-zoom"), step));
                    }
                });
            }
            klass.install_action("win.overview", None, |window, _, _| {
                if let Some(view) = window.imp().dashboard.get() {
                    view.go_home();
                }
            });
            klass.install_action("win.reload", None, |window, _, _| {
                if let Some(view) = window.imp().dashboard.get() {
                    view.reload();
                }
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Window {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.controls_style.load_from_string(&header_css(None));
            gtk::style_context_add_provider_for_display(
                &WidgetExt::display(&*obj),
                &self.controls_style,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            obj.insert_action_group("page", Some(&self.page_actions));
            for popover in obj.popovers() {
                menu::add_arrow_navigation(&popover);
                popover.add_child(&zoom_controls(), "zoom");
                for (custom, entry) in menu::own_entries() {
                    popover.add_child(&menu::row(&entry, &popover), custom);
                }
            }
            for controls in [&*self.start_controls, &*self.end_controls] {
                controls.connect_empty_notify(glib::clone!(
                    #[weak]
                    obj,
                    move |_| obj.update_titlebar()
                ));
            }
        }

        fn dispose(&self) {
            for (object, handler) in self.handlers.take() {
                object.disconnect(handler);
            }
            // The web process otherwise outlives the window.
            if let Some(view) = self.dashboard.get() {
                view.web.terminate_web_process();
            }
            self.dispose_template();
        }
    }

    impl WidgetImpl for Window {}

    impl WindowImpl for Window {
        fn close_request(&self) -> glib::Propagation {
            self.obj().save_window_size();
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

const BASE_CSS: &str = "
popover.menu button.sanctum-menu-row {
  min-height: 32px;
  padding: 0 12px;
  border-radius: 6px;
  font-weight: normal;
}
headerbar.sanctum-compact,
headerbar.sanctum-compact > windowhandle > box {
  min-height: 32px;
  padding-top: 0;
  padding-bottom: 0;
}
headerbar.sanctum-compact > windowhandle > box {
  padding-left: 2px;
  padding-right: 2px;
}
headerbar.sanctum-compact > windowhandle > box > widget > box > button,
headerbar.sanctum-compact > windowhandle > box > widget > box > menubutton > button {
  min-height: 24px;
  min-width: 24px;
  padding: 2px 6px;
}
.sanctum-controls {
  background: var(--sanctum-controls-bg, var(--headerbar-shade-color));
  color: var(--sanctum-controls-fg, var(--headerbar-fg-color));
}
.sanctum-controls.end {
  border-bottom-left-radius: 12px;
}
.sanctum-controls.start {
  border-bottom-right-radius: 12px;
}
";

/// Colors from the frontend's toolbar. While a page loads, the window button
/// section is clear over the launch screen and the header keeps libadwaita's
/// colors.
fn header_css(colors: Option<&HeaderColors>) -> String {
    let mut css = BASE_CSS.to_owned();
    let Some(colors) = colors else {
        css.push_str("window { --sanctum-controls-bg: transparent; }\n");
        return css;
    };
    if let (Some(background), Some(text)) = (colors.background, colors.text) {
        // Older frontends have no primary fill.
        let (fill, on_fill) = match (colors.fill, colors.on_fill) {
            (Some(fill), Some(on_fill)) => (fill.to_string(), on_fill.to_string()),
            _ => (
                format!("color-mix(in oklab, {background}, {text} 12%)"),
                text.to_string(),
            ),
        };
        css.push_str(&format!(
            "window {{ --sanctum-header-bg: {background}; --sanctum-header-fg: {text}; \
             --sanctum-controls-bg: {fill}; --sanctum-controls-fg: {on_fill}; }}
headerbar.sanctum-compact {{ background: var(--sanctum-header-bg); color: var(--sanctum-header-fg); box-shadow: none; }}
"
        ));
    }
    css
}

const ZOOM_STEPS: [f64; 11] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

/// The next step in `direction` from `current`; 0 resets to 100%.
fn next_zoom(current: f64, direction: i32) -> f64 {
    match direction.signum() {
        0 => 1.0,
        1 => ZOOM_STEPS
            .into_iter()
            .find(|step| *step > current + 0.001)
            .unwrap_or(ZOOM_STEPS[ZOOM_STEPS.len() - 1]),
        _ => ZOOM_STEPS
            .into_iter()
            .rev()
            .find(|step| *step < current - 0.001)
            .unwrap_or(ZOOM_STEPS[0]),
    }
}

/// Splits a trailing shortcut hint such as "(Ctrl + K)" or "(A)" off a label,
/// as an accelerator for the menu to show. The page handles the key itself.
fn split_shortcut(label: &str) -> (String, Option<String>) {
    let unchanged = (label.to_owned(), None);
    let Some((text, hint)) = label
        .trim_end()
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
    else {
        return unchanged;
    };
    let mut parts: Vec<&str> = hint.split('+').map(str::trim).collect();
    let Some(key) = parts.pop().filter(|key| !key.is_empty()) else {
        return unchanged;
    };
    let mut accel = String::new();
    for modifier in parts {
        let name = match modifier.to_lowercase().as_str() {
            "ctrl" | "control" | "strg" | "cmd" | "⌘" => "<Control>",
            "shift" | "⇧" => "<Shift>",
            "alt" | "option" | "⌥" => "<Alt>",
            _ => return unchanged,
        };
        accel.push_str(name);
    }
    let mut chars = key.chars();
    let key = match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => c.to_ascii_lowercase().to_string(),
        _ => match key.to_lowercase().as_str() {
            "esc" | "escape" => "Escape".to_owned(),
            "enter" | "return" => "Return".to_owned(),
            "del" | "delete" => "Delete".to_owned(),
            name if name.len() <= 3
                && name.starts_with('f')
                && name[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
            {
                name.to_uppercase()
            }
            _ => return unchanged,
        },
    };
    accel.push_str(&key);
    (text.trim_end().to_owned(), Some(accel))
}

/// A menu label without mnemonics, case or a trailing shortcut hint, so
/// "_Assist" and "Assist (A)" compare equal.
fn menu_label_key(label: &str) -> String {
    let label = label.replace('_', "");
    let label = match label.trim_end().strip_suffix(')') {
        Some(rest) => rest
            .rsplit_once(" (")
            .map_or(label.as_str(), |(head, _)| head),
        None => label.as_str(),
    };
    label.trim().to_lowercase()
}

fn zoom_controls() -> gtk::Box {
    let row = gtk::Box::builder()
        .homogeneous(true)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(4)
        .margin_end(4)
        .css_classes(["linked", "sanctum-zoom"])
        .build();
    let out = gtk::Button::builder()
        .icon_name("zoom-out-symbolic")
        .tooltip_text(gettext("Zoom Out"))
        .action_name("win.zoom-out")
        .build();
    let label = gtk::Label::builder().width_chars(5).build();
    let reset = gtk::Button::builder()
        .child(&label)
        .tooltip_text(gettext("Reset Zoom"))
        .action_name("win.zoom-reset")
        .css_classes(["sanctum-zoom-stop"])
        .build();
    let zoom_in = gtk::Button::builder()
        .icon_name("zoom-in-symbolic")
        .tooltip_text(gettext("Zoom In"))
        .action_name("win.zoom-in")
        .build();
    for button in [&out, &zoom_in] {
        super::label_by_tooltip(button);
    }
    crate::ui::app()
        .settings()
        .bind("web-zoom", &label, "label")
        .get_only()
        .mapping(|value, _| {
            let level = value.get::<f64>()?;
            Some(format!("{:.0}%", level * 100.0).to_value())
        })
        .build();
    row.append(&out);
    row.append(&reset);
    row.append(&zoom_in);
    row
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

#[gtk::template_callbacks]
impl Window {
    #[template_callback]
    fn on_review_certificate_clicked(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let Some(info) = app.client().untrusted_certificate() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                if super::confirm_trust(&window, &info).await {
                    app.trust_certificate(&info);
                    app.client().reconnect();
                }
            }
        ));
    }

    pub fn new(app: &Application) -> Self {
        let window: Self = glib::Object::builder().property("application", app).build();
        window.load_window_size(app.settings());
        window.build_web_view(app);
        window.connect_sources(app);
        window.update_page();
        window.set_registration_alert(app.registration_alert());
        window
    }

    pub fn set_registration_alert(&self, shown: bool) {
        self.imp().registration_banner.set_revealed(shown);
    }

    fn build_web_view(&self, app: &Application) {
        let imp = self.imp();
        let view = DashboardView::new(app);
        imp.web_holder.set_child(Some(&view.widget));
        view.web.connect_title_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |web| {
                let title = web
                    .title()
                    .filter(|title| !title.is_empty())
                    .map(|title| title.to_string())
                    .unwrap_or_else(|| "Sanctum".to_owned());
                window.imp().web_title.set_title(&title);
                window.set_title(Some(&title));
            }
        ));
        view.connect_header_style(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |style| window.apply_header_style(style.as_ref())
        ));
        view.connect_default_dashboard(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |showing| window.action_set_enabled("win.overview", !showing)
        ));
        view.connect_page_menu(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |items| window.set_page_menu(items)
        ));
        view.connect_toolbar(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |toolbar| {
                if window.imp().toolbar.replace(toolbar) != toolbar {
                    window.update_titlebar();
                }
            }
        ));
        let _ = imp.dashboard.set(view);
        self.apply_header_style(None);
        self.update_titlebar();
    }

    /// With the integrated titlebar the frontend's toolbar replaces Sanctum's
    /// header and moves the window. The end section always holds the menu, so
    /// it shows even when the desktop puts no window buttons there.
    pub fn update_titlebar(&self) {
        let imp = self.imp();
        let Some(view) = imp.dashboard.get() else {
            return;
        };
        let wanted = self
            .app()
            .is_some_and(|app| app.settings().boolean("integrated-titlebar"));
        // The page keeps looking for a toolbar while Sanctum's header stands
        // in for a missing one.
        view.set_titlebar(wanted);
        let toolbar = imp.toolbar.get();
        let integrated = wanted && toolbar != Toolbar::Missing;
        imp.main_view.set_reveal_top_bars(!integrated);
        if let Toolbar::Found(height) = toolbar {
            for holder in [&imp.start_controls_box, &imp.end_controls_box] {
                holder.set_height_request(height);
            }
        }

        let no_buttons = imp.end_controls.is_empty();
        imp.end_controls.set_visible(!no_buttons);
        imp.section_menu
            .set_margin_end(if no_buttons { 6 } else { 0 });
        let inset = |holder: &gtk::Box, shown: bool| {
            holder.set_visible(shown);
            if !shown {
                return 0;
            }
            let (_, natural, _, _) = holder.measure(gtk::Orientation::Horizontal, -1);
            natural
        };
        let start = inset(
            &imp.start_controls_box,
            integrated && !imp.start_controls.is_empty(),
        );
        let end = inset(&imp.end_controls_box, integrated);
        view.set_insets(start, end);
    }

    /// The compact header takes the toolbar's colors so the two read as one
    /// bar. The window button section takes the frontend's primary fill, so it
    /// reads as part of Sanctum. Its height is kept per server, so the next start
    /// opens at the toolbar's height.
    fn apply_header_style(&self, style: Option<&HeaderStyle>) {
        let imp = self.imp();
        let Some(app) = self.app() else {
            return;
        };
        let server = app.server();
        let height = match style {
            Some(style) => {
                if server.int("header-height") != style.height {
                    let _ = server.set_int("header-height", style.height);
                }
                style.height
            }
            None => server.int("header-height"),
        };
        if height > 0 && !matches!(imp.toolbar.get(), Toolbar::Found(_)) {
            for holder in [&imp.start_controls_box, &imp.end_controls_box] {
                holder.set_height_request(height);
            }
        }
        imp.controls_style
            .load_from_string(&header_css(style.map(|style| &style.colors)));
        if let Some(primary) = style.and_then(|style| style.colors.primary) {
            let value = primary.to_string();
            if server.string("accent-color") != value {
                let _ = server.set_string("accent-color", &value);
            }
        }
    }

    fn view(&self) -> &Rc<DashboardView> {
        self.imp().dashboard.get().expect("created in new")
    }

    /// The page starts loading once Sanctum knows the server address, while
    /// Sanctum's WebSocket may still be connecting.
    fn open_web_view(&self, app: &Application) {
        let view = self.view();
        let base = app.client().active_url();
        if base.is_empty() {
            return;
        }
        view.set_base(&base);
        if view.is_open() {
            return;
        }
        view.open(&app.server().string("last-path"));
    }

    /// Opens a server path such as `/lovelace/cameras`, for notification
    /// clicks and `command_webview`.
    pub fn show_web_path(&self, path: &str) {
        self.view().open(path);
    }

    pub fn set_zoom(&self, level: f64) {
        self.view().set_zoom(level);
        self.update_titlebar();
    }

    pub fn apply_rendering(&self) {
        self.view().apply_rendering();
    }

    fn popovers(&self) -> Vec<gtk::PopoverMenu> {
        let imp = self.imp();
        [&*imp.start_menu, &*imp.header_menu, &*imp.section_menu]
            .into_iter()
            .filter_map(|button| button.popover().and_downcast::<gtk::PopoverMenu>())
            .collect()
    }

    /// Puts the toolbar's overflow entries under Default Dashboard, which the
    /// page hides from its toolbar meanwhile. Entries Sanctum's menu already
    /// has, such as Assist, are left out.
    fn set_page_menu(&self, items: Option<Vec<PageMenuItem>>) {
        let imp = self.imp();
        let Some(home) = imp
            .section_menu
            .menu_model()
            .and_then(|model| model.item_link(1, gio::MENU_LINK_SECTION))
            .and_downcast::<gio::Menu>()
        else {
            return;
        };
        // Removing the section before its rows logged
        // gtk_menu_section_box_remove_custom criticals.
        for (popover, row) in imp.page_rows.take() {
            popover.remove_child(&row);
        }
        if imp.page_section.replace(false) {
            home.remove(1);
        }
        for name in imp.page_actions.list_actions() {
            imp.page_actions.remove_action(&name);
        }
        let Some(items) = items.filter(|items| !items.is_empty()) else {
            return;
        };
        let own: Vec<String> = menu::own_entries()
            .iter()
            .map(|(_, entry)| menu_label_key(&entry.label))
            .collect();
        let popovers = self.popovers();
        let mut rows = Vec::new();
        let sections = gio::Menu::new();
        let mut section = gio::Menu::new();
        for item in items {
            match item {
                PageMenuItem::Separator => {
                    if section.n_items() > 0 {
                        sections.append_section(None, &section);
                        section = gio::Menu::new();
                    }
                }
                PageMenuItem::Item {
                    id,
                    label,
                    icon,
                    disabled,
                    checked,
                } => {
                    let (label, accel) = split_shortcut(&label);
                    if own.contains(&menu_label_key(&label)) {
                        continue;
                    }
                    let name = format!("item-{id}");
                    let action = gio::SimpleAction::new(&name, None);
                    action.set_enabled(!disabled);
                    action.connect_activate(glib::clone!(
                        #[weak(rename_to = window)]
                        self,
                        move |_, _| window.view().select_page_menu_item(id)
                    ));
                    imp.page_actions.add_action(&action);
                    let entry = menu::Entry {
                        label,
                        icon,
                        icon_name: None,
                        action: format!("page.{name}"),
                        accel,
                        checked,
                        hide_disabled: false,
                    };
                    let custom = format!("page-{id}");
                    let item = gio::MenuItem::new(None, None);
                    item.set_attribute_value("custom", Some(&custom.to_variant()));
                    section.append_item(&item);
                    rows.push((entry, custom));
                }
            }
        }
        if section.n_items() > 0 {
            sections.append_section(None, &section);
        }
        if rows.is_empty() {
            return;
        }
        home.append_section(None, &sections);
        imp.page_section.set(true);
        let mut added = Vec::new();
        for popover in popovers {
            for (entry, custom) in &rows {
                let row = menu::row(entry, &popover);
                popover.add_child(&row, custom);
                added.push((popover.clone(), row.upcast::<gtk::Widget>()));
            }
        }
        imp.page_rows.replace(added);
    }

    pub fn apply_swipe_navigation(&self) {
        self.view().apply_swipe_navigation();
    }

    pub fn update_page_settings(&self) {
        self.view().update_page_settings();
    }

    fn connect_sources(&self, app: &Application) {
        let client = app.client();
        let mut handlers = Vec::new();
        for property in [
            "state",
            "error-message",
            "location-name",
            "ha-version",
            "active-url",
        ] {
            let handler = client.connect_notify_local(
                Some(property),
                glib::clone!(
                    #[weak(rename_to = window)]
                    self,
                    move |_, _| window.update_page()
                ),
            );
            handlers.push((client.clone().upcast(), handler));
        }
        let handler = app.settings().connect_changed(
            Some("server"),
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| window.update_page()
            ),
        );
        handlers.push((app.settings().clone().upcast(), handler));
        let handler = app.settings().connect_changed(
            Some("integrated-titlebar"),
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| window.update_titlebar()
            ),
        );
        handlers.push((app.settings().clone().upcast(), handler));
        let settings = gtk::Settings::for_display(&WidgetExt::display(self));
        let handler = settings.connect_gtk_decoration_layout_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_| {
                // The controls rebuild their buttons on the same signal, so
                // they are measured afterwards.
                glib::idle_add_local_once(glib::clone!(
                    #[weak]
                    window,
                    move || window.update_titlebar()
                ));
            }
        ));
        handlers.push((settings.upcast(), handler));
        self.imp().handlers.replace(handlers);
    }

    fn app(&self) -> Option<Application> {
        self.application().and_downcast()
    }

    pub fn update_page(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let imp = self.imp();
        let client = app.client();
        let current = imp.stack.visible_child_name().unwrap_or_default();

        let page = if app.is_loading() {
            "loading"
        } else if !app.is_signed_in() {
            if current != "onboarding" {
                imp.onboarding.reset();
            }
            "onboarding"
        } else if imp.onboarding.is_presenting() {
            self.open_web_view(&app);
            "onboarding"
        } else {
            self.open_web_view(&app);
            match client.state() {
                ConnectionState::Connected => "main",
                ConnectionState::Disconnected => {
                    self.update_disconnected_page(&client.error_message());
                    "disconnected"
                }
                ConnectionState::AuthFailed => {
                    if current != "onboarding" {
                        if let Some(credentials) = client.credentials() {
                            imp.onboarding.reauthenticate(credentials);
                        }
                        self.show_toast(&gettext("Sign in again to continue"));
                    }
                    "onboarding"
                }
                // A retry keeps the current page so the spinner does not flash.
                ConnectionState::Connecting
                    if matches!(current.as_str(), "main" | "disconnected") =>
                {
                    current.as_str()
                }
                ConnectionState::Connecting | ConnectionState::Idle => "loading",
            }
        };
        if current != page {
            imp.stack.set_visible_child_name(page);
        }
    }

    fn update_disconnected_page(&self, error: &str) {
        let Some(app) = self.app() else {
            return;
        };
        let imp = self.imp();
        let client = app.client();
        let mut server = client.active_url();
        if server.is_empty() {
            server = app.server().string("url").to_string();
        }
        let untrusted = client.untrusted_certificate().is_some();
        let status = if untrusted {
            gettext("The server's certificate is not trusted")
        } else {
            gettext("Retrying automatically")
        };
        let lead = glib::markup_escape_text(&format!("{status}\n{server}")).to_string();
        let description = if error.is_empty() || untrusted {
            lead
        } else {
            format!(
                "{lead}\n\n<small>{}</small>",
                glib::markup_escape_text(error)
            )
        };
        imp.disconnected_page.set_description(Some(&description));

        imp.certificate_button.set_visible(untrusted);
        if untrusted {
            imp.retry_button.remove_css_class("suggested-action");
        } else {
            imp.retry_button.add_css_class("suggested-action");
        }
    }

    /// Toast titles are one line, so error detail lines are joined.
    pub fn show_toast(&self, message: &str) {
        let title = message.replace('\n', ": ");
        let toast = adw::Toast::builder()
            .title(&title)
            .use_markup(false)
            .build();
        // Toasts on the window are hidden behind an open Preferences dialog.
        match self
            .visible_dialog()
            .and_downcast::<adw::PreferencesDialog>()
        {
            Some(dialog) => dialog.add_toast(toast),
            None => self.imp().toast_overlay.add_toast(toast),
        }
    }

    fn load_window_size(&self, settings: &gio::Settings) {
        self.set_default_size(settings.int("window-width"), settings.int("window-height"));
        if settings.boolean("is-maximized") {
            self.maximize();
        }
    }

    fn save_window_size(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let settings = app.settings();
        let (width, height) = self.default_size();
        let _ = settings.set_int("window-width", width);
        let _ = settings.set_int("window-height", height);
        let _ = settings.set_boolean("is-maximized", self.is_maximized());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_shortcuts() {
        assert_eq!(
            split_shortcut("Search Home Assistant (Ctrl + K)"),
            (
                "Search Home Assistant".to_owned(),
                Some("<Control>k".to_owned())
            )
        );
        assert_eq!(
            split_shortcut("Assist (A)"),
            ("Assist".to_owned(), Some("a".to_owned()))
        );
        assert_eq!(
            split_shortcut("Edit overview"),
            ("Edit overview".to_owned(), None)
        );
        assert_eq!(
            split_shortcut("Lights (Kitchen)"),
            ("Lights (Kitchen)".to_owned(), None)
        );
    }

    #[test]
    fn compares_menu_labels() {
        assert_eq!(menu_label_key("_Assist"), "assist");
        assert_eq!(menu_label_key("Assist (A)"), "assist");
        assert_eq!(
            menu_label_key("Search Home Assistant"),
            "search home assistant"
        );
        assert_eq!(menu_label_key("(Unnamed)"), "(unnamed)");
    }

    #[test]
    fn steps_zoom() {
        assert_eq!(next_zoom(1.0, 1), 1.1);
        assert_eq!(next_zoom(1.0, -1), 0.9);
        assert_eq!(next_zoom(2.0, 1), 2.0);
        assert_eq!(next_zoom(0.5, -1), 0.5);
        assert_eq!(next_zoom(1.3, -1), 1.25);
        assert_eq!(next_zoom(1.7, 0), 1.0);
    }
}
