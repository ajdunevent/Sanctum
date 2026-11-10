//! The Home Assistant frontend in WebKit, which is Sanctum's main interface.
//! The frontend signs in through its external app protocol, the one the iOS
//! app uses, so it shares Sanctum's token and stores none.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib};
use serde_json::{Value, json};
use webkit::prelude::*;

use super::rendering::Mode;
use crate::application::Application;
use crate::ha::trust;

/// How long a token request waits for the sign-in to be available.
const AUTH_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

const GET_AUTH: &str = "getExternalAuth";
const REVOKE_AUTH: &str = "revokeExternalAuth";
const BUS: &str = "externalBus";
const PAGE: &str = "sanctum";

const PAGE_SCRIPT: &str = include_str!("page.js");

const HEADER_STYLE: &str = r#"(() => {
  const style = getComputedStyle(document.documentElement);
  window.webkit.messageHandlers.sanctum.postMessage({
    type: "header",
    text: style.getPropertyValue("--app-header-text-color").trim(),
    background: style.getPropertyValue("--app-header-background-color").trim(),
    height: parseFloat(style.getPropertyValue("--header-height")) || 56,
    width: innerWidth,
    primary: style.getPropertyValue("--primary-color").trim(),
    fill: style.getPropertyValue("--ha-color-fill-primary-normal-resting").trim(),
    onFill: style.getPropertyValue("--ha-color-on-primary-normal").trim(),
  });
})();"#;

/// The frontend chooses external auth only when the address asks for it, so
/// a full reload or link would otherwise fall back to its login page. Links to
/// other sites never load here, so the top frame is always the server.
const KEEP_EXTERNAL_AUTH: &str = r#"if (!location.search.includes("external_auth=1")) {
  const url = new URL(location.href);
  url.searchParams.set("external_auth", "1");
  history.replaceState(history.state, "", url);
}"#;

/// Frames per second and the longest frame over the last half second, as the
/// page sees them through animation frames.
const FRAME_RATE: &str = r#"(() => {
  const meter = document.createElement("div");
  meter.style.cssText = "position:fixed;z-index:2147483647;pointer-events:none;" +
    "top:calc(var(--header-height, 56px) + 4px);right:4px;padding:2px 6px;border-radius:4px;" +
    "font:12px monospace;color:#fff;background:rgb(0 0 0 / 70%)";
  let frames = 0, worst = 0, last = performance.now(), start = last;
  const tick = (now) => {
    frames++;
    worst = Math.max(worst, now - last);
    last = now;
    if (now - start >= 500) {
      meter.textContent = `${Math.round(frames * 1000 / (now - start))} fps · ${Math.round(worst)} ms`;
      frames = 0; worst = 0; start = now;
    }
    requestAnimationFrame(tick);
  };
  const show = () => { document.body.append(meter); requestAnimationFrame(tick); };
  document.body ? show() : addEventListener("DOMContentLoaded", show);
})();"#;

/// WebKit aborts the web process when a view transition starts while
/// hardware acceleration is off. The frontend skips transitions when the API
/// is missing, which Reduce Motion uses too.
const NO_VIEW_TRANSITIONS: &str = "delete Document.prototype.startViewTransition;";

/// Cuts the motion the frontend keeps under prefers-reduced-motion. Styles in
/// a shadow root ignore the document's, so the sheet is added to every root.
/// Durations stay above zero, since zero-length transitions never fire
/// transitionend and some components wait for it.
const REDUCE_MOTION: &str = r#"(() => {
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(`*, *::before, *::after {
    animation-duration: 0.01ms !important; animation-delay: 0s !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important; transition-delay: 0s !important;
    scroll-behavior: auto !important; }`);
  const withSheet = (sheets) => sheets.includes(sheet) ? sheets : [...sheets, sheet];
  for (const proto of [ShadowRoot.prototype, Document.prototype]) {
    const property = Object.getOwnPropertyDescriptor(proto, "adoptedStyleSheets");
    if (!property) continue;
    Object.defineProperty(proto, "adoptedStyleSheets", {
      ...property,
      set(sheets) { property.set.call(this, withSheet(sheets)); },
    });
  }
  const attachShadow = Element.prototype.attachShadow;
  Element.prototype.attachShadow = function (...args) {
    const root = attachShadow.apply(this, args);
    root.adoptedStyleSheets = root.adoptedStyleSheets;
    return root;
  };
  document.adoptedStyleSheets = document.adoptedStyleSheets;
  const animate = Element.prototype.animate;
  Element.prototype.animate = function (keyframes, options) {
    const timing = typeof options === "number" ? {} : { ...options };
    return animate.call(this, keyframes, { ...timing, duration: 0, delay: 0, endDelay: 0, iterations: 1 });
  };
})();"#;

/// Scripts every Home Assistant page needs, the sign-in page included.
pub fn add_compatibility_scripts(content: &webkit::UserContentManager) {
    let app = crate::ui::app();
    let settings = app.settings();
    if Mode::current(settings) == Mode::Software || settings.boolean("web-reduce-motion") {
        content.add_script(&webkit::UserScript::new(
            NO_VIEW_TRANSITIONS,
            webkit::UserContentInjectedFrames::AllFrames,
            webkit::UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
    }
    if settings.boolean("web-reduce-motion") {
        content.add_script(&webkit::UserScript::new(
            REDUCE_MOTION,
            webkit::UserContentInjectedFrames::TopFrame,
            webkit::UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
    }
}

thread_local! {
    static SESSION: OnceCell<webkit::NetworkSession> = const { OnceCell::new() };
}

/// Cookies and frontend settings such as the theme are kept between runs.
fn network_session() -> webkit::NetworkSession {
    SESSION.with(|session| {
        session
            .get_or_init(|| {
                let dir = |base: std::path::PathBuf| {
                    base.join("sanctum")
                        .join("web")
                        .to_string_lossy()
                        .into_owned()
                };
                webkit::NetworkSession::new(
                    Some(&dir(glib::user_data_dir())),
                    Some(&dir(glib::user_cache_dir())),
                )
            })
            .clone()
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HeaderColors {
    /// The frontend's primary color, which Sanctum uses as its accent.
    pub primary: Option<gdk::RGBA>,
    pub text: Option<gdk::RGBA>,
    pub background: Option<gdk::RGBA>,
    /// The frontend's quiet primary fill: a pale tint of the primary color in
    /// light mode and a deep shade in dark mode.
    pub fill: Option<gdk::RGBA>,
    /// The frontend's foreground for that fill.
    pub on_fill: Option<gdk::RGBA>,
}

#[derive(Clone, Copy, Debug)]
pub struct HeaderStyle {
    pub colors: HeaderColors,
    /// In GTK pixels.
    pub height: i32,
}

/// `None` while a page loads, before the frontend draws its toolbar.
type HeaderHandler = Box<dyn Fn(Option<HeaderStyle>)>;

/// Whether the page has a toolbar the integrated titlebar can use.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Toolbar {
    /// Not reported yet, as while the page loads.
    #[default]
    Unknown,
    /// Its height in GTK pixels.
    Found(i32),
    Missing,
}

type ToolbarHandler = Box<dyn Fn(Toolbar)>;

/// An entry of the toolbar's overflow menu, which Sanctum's menu takes over.
#[derive(Clone, Debug, PartialEq)]
pub enum PageMenuItem {
    Separator,
    Item {
        id: u64,
        label: String,
        /// SVG path data of the entry's icon.
        icon: Option<String>,
        disabled: bool,
        /// `Some` for a check item, with its state.
        checked: Option<bool>,
    },
}

/// `None` when the page has no overflow menu Sanctum can take over.
type PageMenuHandler = Box<dyn Fn(Option<Vec<PageMenuItem>>)>;

type DefaultDashboardHandler = Box<dyn Fn(bool)>;

fn page_menu(value: &Value) -> Option<Vec<PageMenuItem>> {
    let items = value.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                if item["separator"].as_bool() == Some(true) {
                    return Some(PageMenuItem::Separator);
                }
                Some(PageMenuItem::Item {
                    id: item["id"].as_u64()?,
                    label: item["label"].as_str()?.to_owned(),
                    icon: item["icon"].as_str().map(str::to_owned),
                    disabled: item["disabled"].as_bool().unwrap_or(false),
                    checked: (item["checkbox"].as_bool() == Some(true))
                        .then(|| item["checked"].as_bool().unwrap_or(false)),
                })
            })
            .collect(),
    )
}

/// The toolbar as the page reports it, with the height in CSS pixels.
#[derive(Clone, Copy, Debug)]
struct CssHeader {
    colors: HeaderColors,
    height: f64,
}

pub struct DashboardView {
    /// The page with its loading bar and error screen.
    pub widget: gtk::Widget,
    pub web: webkit::WebView,
    content: webkit::UserContentManager,
    stack: gtk::Stack,
    error: adw::StatusPage,
    /// The path last opened, such as `/lovelace/0`, reloaded on a new base URL.
    path: RefCell<String>,
    base: RefCell<String>,
    titlebar: Cell<bool>,
    /// Space kept free for window buttons at the start and end of the toolbar.
    insets: Cell<(i32, i32)>,
    /// The inset widths as asked for, in GTK pixels.
    requested_insets: Cell<(i32, i32)>,
    /// GTK pixels per CSS pixel. WebKit can draw the page larger than the zoom
    /// level says, as on Phosh with text scaling, so it is measured from the
    /// page's width.
    scale: Cell<f64>,
    /// The last toolbar style in CSS pixels, sent again when the scale changes.
    header: RefCell<Option<CssHeader>>,
    /// The last button press, which moving the window and its menu need.
    press: RefCell<Option<Press>>,
    header_style: RefCell<Option<HeaderHandler>>,
    /// The page's toolbar as last reported, with its height in CSS pixels.
    toolbar: Cell<Option<(bool, f64)>>,
    toolbar_handler: RefCell<Option<ToolbarHandler>>,
    page_menu_handler: RefCell<Option<PageMenuHandler>>,
    default_dashboard_handler: RefCell<Option<DefaultDashboardHandler>>,
    scanner: RefCell<Option<Rc<super::scanner::Scanner>>>,
}

struct Press {
    event: gdk::Event,
    /// Position in the view.
    x: f64,
    y: f64,
}

impl std::fmt::Debug for DashboardView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DashboardView")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Callback names come from the page, so only plain identifiers are called.
fn identifier(name: &str) -> Option<&str> {
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then_some(name)
}

/// The URL that starts the frontend in external auth mode.
pub fn start_url(base: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    let separator = if path.contains('?') { '&' } else { '?' };
    format!(
        "{}/{path}{separator}external_auth=1",
        base.trim_end_matches('/')
    )
}

/// Settings shared with the sign-in view.
pub fn apply_settings(web: &webkit::WebView) {
    let Some(settings) = webkit::prelude::WebViewExt::settings(web) else {
        return;
    };
    let devel = crate::config::PROFILE == "Devel";
    settings.set_enable_developer_extras(devel);
    settings.set_enable_write_console_messages_to_stdout(devel);
    settings.set_user_agent_with_application_details(Some("Sanctum"), Some(crate::config::VERSION));
    apply_rendering(&settings);
    web.set_zoom_level(crate::ui::app().settings().double("web-zoom"));
}

/// The dashboard only; the sign-in view has no history to swipe through.
fn apply_swipe_navigation(web: &webkit::WebView) {
    if let Some(settings) = webkit::prelude::WebViewExt::settings(web) {
        settings.set_enable_back_forward_navigation_gestures(
            crate::ui::app().settings().boolean("swipe-navigation"),
        );
    }
}

fn apply_rendering(web_settings: &webkit::Settings) {
    let app = crate::ui::app();
    let settings = app.settings();
    web_settings.set_hardware_acceleration_policy(Mode::current(settings).policy());
    web_settings.set_enable_smooth_scrolling(!settings.boolean("web-reduce-motion"));
}

/// Stock entries that act on text, links and images. Navigation entries
/// would leave the frontend's history.
fn keeps_menu_entry(action: webkit::ContextMenuAction) -> bool {
    use webkit::ContextMenuAction as A;
    matches!(
        action,
        A::Copy
            | A::Cut
            | A::Paste
            | A::Delete
            | A::SelectAll
            | A::CopyLinkToClipboard
            | A::CopyImageToClipboard
            | A::CopyImageUrlToClipboard
            | A::SpellingGuess
            | A::NoGuessesFound
            | A::IgnoreSpelling
            | A::LearnSpelling
            | A::InsertEmoji
            | A::InspectElement
    )
}

/// Pages that handle right clicks themselves cancel the event, and WebKit
/// then shows no menu, so only the default menu is trimmed here.
fn trim_context_menu(menu: &webkit::ContextMenu) {
    for item in menu.items() {
        if !item.is_separator() && !keeps_menu_entry(item.stock_action()) {
            menu.remove(&item);
        }
    }
    let mut previous_separator = true;
    for item in menu.items() {
        if item.is_separator() && previous_separator {
            menu.remove(&item);
        }
        previous_separator = item.is_separator();
    }
    if menu.last().is_some_and(|item| !item.is_separator()) {
        menu.append(&webkit::ContextMenuItem::new_separator());
    }
    menu.append(&webkit::ContextMenuItem::from_stock_action(
        webkit::ContextMenuAction::Reload,
    ));
}

fn parse_color(value: &Value) -> Option<gdk::RGBA> {
    gdk::RGBA::parse(value.as_str()?.trim()).ok()
}

fn origin(uri: &str) -> Option<String> {
    let uri = glib::Uri::parse(uri, glib::UriFlags::NONE).ok()?;
    Some(trust::host_key(&uri))
}

impl DashboardView {
    pub fn new(app: &Application) -> Rc<Self> {
        let content = webkit::UserContentManager::new();
        for name in [GET_AUTH, REVOKE_AUTH, BUS, PAGE] {
            content.register_script_message_handler(name, None);
        }
        let web = webkit::WebView::builder()
            .network_session(&network_session())
            .user_content_manager(&content)
            .vexpand(true)
            .build();
        apply_settings(&web);
        apply_swipe_navigation(&web);

        let progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Start)
            .css_classes(["osd"])
            .visible(false)
            .build();
        let overlay = gtk::Overlay::builder().child(&web).build();
        overlay.add_overlay(&progress);

        let error = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title(gettext("Home Assistant Unavailable"))
            .build();
        let retry = gtk::Button::builder()
            .label(gettext("_Retry"))
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        error.set_child(Some(&retry));

        let stack = gtk::Stack::new();
        stack.add_named(&overlay, Some("web"));
        stack.add_named(&error, Some("error"));

        let this = Rc::new(Self {
            widget: stack.clone().upcast(),
            web: web.clone(),
            content: content.clone(),
            stack,
            error,
            path: RefCell::default(),
            base: RefCell::new(app.client().active_url()),
            titlebar: Cell::new(false),
            insets: Cell::new((0, 0)),
            requested_insets: Cell::new((0, 0)),
            scale: Cell::new(0.0),
            header: RefCell::default(),
            press: RefCell::default(),
            header_style: RefCell::default(),
            toolbar: Cell::default(),
            toolbar_handler: RefCell::default(),
            page_menu_handler: RefCell::default(),
            default_dashboard_handler: RefCell::default(),
            scanner: RefCell::default(),
        });
        this.update_user_content();
        // Capture runs before WebKit forwards the event to the page, so the
        // press is stored before the page can ask for it.
        let legacy = gtk::EventControllerLegacy::new();
        legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
        legacy.connect_event({
            let weak = Rc::downgrade(&this);
            move |controller, event| {
                if event.event_type() == gdk::EventType::ButtonPress {
                    if let Some(this) = weak.upgrade() {
                        this.store_press(controller.widget().as_ref(), event);
                    }
                }
                glib::Propagation::Proceed
            }
        });
        web.add_controller(legacy);
        web.connect_context_menu(|_, menu, _| {
            trim_context_menu(menu);
            false
        });

        let weak = Rc::downgrade(&this);
        retry.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });

        web.connect_estimated_load_progress_notify(glib::clone!(
            #[weak]
            progress,
            move |web| {
                let fraction = web.estimated_load_progress();
                progress.set_fraction(fraction);
                progress.set_visible(fraction < 1.0);
            }
        ));
        web.connect_load_changed({
            let weak = Rc::downgrade(&this);
            move |_, event| {
                if event == webkit::LoadEvent::Started
                    && let Some(this) = weak.upgrade()
                {
                    this.on_load_started();
                }
            }
        });
        web.connect_uri_notify({
            let weak = Rc::downgrade(&this);
            move |web| {
                if let (Some(this), Some(uri)) = (weak.upgrade(), web.uri()) {
                    this.remember_path(&uri);
                }
            }
        });
        web.connect_load_failed({
            let weak = Rc::downgrade(&this);
            move |_, _, uri, error| {
                if error.matches(webkit::NetworkError::Cancelled)
                    || error.matches(webkit::PolicyError::FrameLoadInterruptedByPolicyChange)
                {
                    return false;
                }
                if let Some(this) = weak.upgrade() {
                    this.show_error(&format!("{uri}\n{}", error.message()));
                }
                true
            }
        });
        web.connect_web_process_terminated({
            let weak = Rc::downgrade(&this);
            move |_, reason| {
                if reason == webkit::WebProcessTerminationReason::TerminatedByApi {
                    return;
                }
                log::warn!("Dashboard web process ended: {reason:?}");
                if let Some(this) = weak.upgrade() {
                    this.show_error(&gettext("The page stopped unexpectedly"));
                }
            }
        });
        web.connect_load_failed_with_tls_errors({
            let weak = Rc::downgrade(&this);
            move |web, uri, certificate, errors| {
                let Some(this) = weak.upgrade() else {
                    return false;
                };
                let Ok(parsed) = glib::Uri::parse(uri, glib::UriFlags::NONE) else {
                    return false;
                };
                let info = trust::describe(trust::host_key(&parsed), certificate, errors);
                if trust::is_trusted(&info) {
                    if let Some(session) = web.network_session() {
                        session.allow_tls_certificate_for_host(
                            certificate,
                            &parsed.host().unwrap_or_default(),
                        );
                    }
                    this.reload();
                } else {
                    this.show_error(&gettext(
                        "The server's certificate is not trusted. Review it from the Not Connected screen.",
                    ));
                }
                true
            }
        });
        web.connect_decide_policy({
            let weak = Rc::downgrade(&this);
            move |_, decision, kind| {
                let Some(this) = weak.upgrade() else {
                    return false;
                };
                this.decide(decision, kind)
            }
        });
        web.connect_create(|_, action| {
            if let Some(uri) = action.clone().request().and_then(|r| r.uri()) {
                launch(&uri);
            }
            None
        });

        let weak = Rc::downgrade(&this);
        content.connect_script_message_received(Some(GET_AUTH), move |_, value| {
            if let Some(this) = weak.upgrade() {
                this.on_get_auth(parse(value));
            }
        });
        let weak = Rc::downgrade(&this);
        content.connect_script_message_received(Some(REVOKE_AUTH), move |_, value| {
            if let Some(this) = weak.upgrade() {
                this.on_revoke_auth(parse(value));
            }
        });
        let weak = Rc::downgrade(&this);
        content.connect_script_message_received(Some(BUS), move |_, value| {
            if let Some(this) = weak.upgrade() {
                this.on_bus(parse(value));
            }
        });
        let weak = Rc::downgrade(&this);
        content.connect_script_message_received(Some(PAGE), move |_, value| {
            if let Some(this) = weak.upgrade() {
                this.on_page(parse(value));
            }
        });
        this
    }

    fn update_user_content(&self) {
        let content = &self.content;
        content.remove_all_scripts();
        let script = |source: &str| {
            webkit::UserScript::new(
                source,
                webkit::UserContentInjectedFrames::TopFrame,
                webkit::UserScriptInjectionTime::Start,
                &[],
                &[],
            )
        };
        add_compatibility_scripts(content);
        content.add_script(&script(KEEP_EXTERNAL_AUTH));
        content.add_script(&script(&self.titlebar_script()));
        content.add_script(&script(PAGE_SCRIPT));
        if crate::ui::app().settings().boolean("web-show-fps") {
            content.add_script(&script(FRAME_RATE));
        }
    }

    /// Runs before the page script on a load, and again after it on a change.
    fn titlebar_script(&self) -> String {
        let (start, end) = self.insets.get();
        let pull = crate::ui::app().settings().boolean("pull-to-refresh");
        format!(
            "window.sanctumTitlebar = {}; window.sanctumInsets = {{ start: {start}, end: {end} }}; \
             window.sanctumPullToRefresh = {pull}; window.sanctumPadToolbars?.(); undefined;",
            self.titlebar.get()
        )
    }

    pub fn apply_swipe_navigation(&self) {
        apply_swipe_navigation(&self.web);
    }

    /// Sends a changed page setting, such as pull to refresh, to the page.
    pub fn update_page_settings(&self) {
        self.update_user_content();
        self.run(self.titlebar_script());
    }

    pub fn set_titlebar(&self, on: bool) {
        if self.titlebar.replace(on) == on {
            return;
        }
        self.update_user_content();
        self.run(self.titlebar_script());
    }

    /// Takes widths in GTK pixels.
    pub fn set_insets(&self, start: i32, end: i32) {
        self.requested_insets.set((start, end));
        self.apply_insets();
    }

    fn scale(&self) -> f64 {
        match self.scale.get() {
            scale if scale > 0.0 => scale,
            _ => self.web.zoom_level().max(0.1),
        }
    }

    fn apply_insets(&self) {
        let scale = self.scale();
        let css = |px: i32| (f64::from(px) / scale).ceil() as i32;
        let (start, end) = self.requested_insets.get();
        let (start, end) = (css(start), css(end));
        if self.insets.replace((start, end)) == (start, end) {
            return;
        }
        self.update_user_content();
        self.run(self.titlebar_script());
    }

    fn update_scale(&self, css_width: f64) {
        let width = f64::from(self.web.width());
        if css_width <= 0.0 || width <= 0.0 {
            return;
        }
        let scale = width / css_width;
        if (scale - self.scale.get()).abs() < 0.005 {
            return;
        }
        log::debug!("Page scale {scale:.3}");
        self.scale.set(scale);
        self.apply_insets();
        self.emit_header();
        self.emit_toolbar();
    }

    /// The toolbar's size on screen changes with the zoom, so it is measured
    /// again.
    pub fn set_zoom(&self, level: f64) {
        self.web.set_zoom_level(level);
        self.run(HEADER_STYLE.to_owned());
    }

    pub fn connect_header_style(&self, f: impl Fn(Option<HeaderStyle>) + 'static) {
        self.header_style.replace(Some(Box::new(f)));
    }

    /// Called with true while the page shows the default dashboard.
    pub fn connect_default_dashboard(&self, f: impl Fn(bool) + 'static) {
        self.default_dashboard_handler.replace(Some(Box::new(f)));
    }

    pub fn connect_page_menu(&self, f: impl Fn(Option<Vec<PageMenuItem>>) + 'static) {
        self.page_menu_handler.replace(Some(Box::new(f)));
    }

    /// Chooses an entry of the overflow menu Sanctum took over.
    pub fn select_page_menu_item(&self, id: u64) {
        self.run(format!("window.sanctumMenuSelect?.({id}); undefined;"));
    }

    pub fn connect_toolbar(&self, f: impl Fn(Toolbar) + 'static) {
        self.toolbar_handler.replace(Some(Box::new(f)));
    }

    fn emit_toolbar(&self) {
        let toolbar = match self.toolbar.get() {
            None => Toolbar::Unknown,
            Some((false, _)) => Toolbar::Missing,
            Some((true, height)) => Toolbar::Found((height * self.scale()).round() as i32),
        };
        if let Some(f) = self.toolbar_handler.borrow().as_ref() {
            f(toolbar);
        }
    }

    fn store_press(&self, widget: Option<&gtk::Widget>, event: &gdk::Event) {
        let (Some(widget), Some((x, y))) = (widget, event.position()) else {
            return;
        };
        let Some(native) = widget.native() else {
            return;
        };
        let (offset_x, offset_y) = native.surface_transform();
        let point = native
            .compute_point(
                widget,
                &gtk::graphene::Point::new((x - offset_x) as f32, (y - offset_y) as f32),
            )
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        self.press.replace(Some(Press {
            event: event.clone(),
            x: point.x().into(),
            y: point.y().into(),
        }));
    }

    fn on_page(&self, message: Value) {
        match message["type"].as_str().unwrap_or_default() {
            "refresh" => self.reload(),
            "header" => self.on_header_style(&message),
            "viewport" => self.update_scale(message["width"].as_f64().unwrap_or_default()),
            "default-dashboard" => {
                if let Some(f) = self.default_dashboard_handler.borrow().as_ref() {
                    f(message["showing"].as_bool().unwrap_or(false));
                }
            }
            "menu" => {
                if let Some(f) = self.page_menu_handler.borrow().as_ref() {
                    f(page_menu(&message["items"]));
                }
            }
            "toolbar" => {
                self.toolbar.set(Some((
                    message["found"].as_bool().unwrap_or(true),
                    message["height"].as_f64().unwrap_or_default(),
                )));
                self.emit_toolbar();
            }
            "titlebar" if self.titlebar.get() => {
                self.titlebar_action(message["action"].as_str().unwrap_or_default());
            }
            _ => {}
        }
    }

    fn titlebar_action(&self, action: &str) {
        let Some(window) = self.web.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let settings = gtk::Settings::default();
        let setting = |name: &str| -> String {
            settings
                .as_ref()
                .map(|settings| {
                    settings
                        .property::<Option<String>>(name)
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        };
        let behavior = match action {
            "move" => {
                log::debug!("Titlebar move");
                self.begin_move(&window);
                return;
            }
            "double" => setting("gtk-titlebar-double-click"),
            "middle" => setting("gtk-titlebar-middle-click"),
            "right" => setting("gtk-titlebar-right-click"),
            _ => return,
        };
        log::debug!("Titlebar {action}: {behavior}");
        match behavior.as_str() {
            "toggle-maximize" | "toggle-maximize-horizontally" | "toggle-maximize-vertically" => {
                window.set_maximized(!window.is_maximized());
            }
            "minimize" => window.minimize(),
            "lower" => {
                if let Some(toplevel) = window.surface().and_downcast::<gdk::Toplevel>() {
                    toplevel.lower();
                }
            }
            "menu" => self.show_window_menu(&window),
            _ => {}
        }
    }

    fn begin_move(&self, window: &gtk::Window) {
        let press = self.press.borrow();
        let (Some(press), Some(toplevel)) = (
            press.as_ref(),
            window.surface().and_downcast::<gdk::Toplevel>(),
        ) else {
            return;
        };
        let (Some(device), Some((x, y))) = (press.event.device(), press.event.position()) else {
            return;
        };
        let button = press
            .event
            .downcast_ref::<gdk::ButtonEvent>()
            .map_or(1, gdk::ButtonEvent::button);
        toplevel.begin_move(&device, button as i32, x, y, press.event.time());
    }

    /// Compositors without a window menu get GTK's window actions in a popover.
    fn show_window_menu(&self, window: &gtk::Window) {
        let press = self.press.borrow();
        let Some(press) = press.as_ref() else {
            return;
        };
        if let Some(toplevel) = window.surface().and_downcast::<gdk::Toplevel>() {
            if toplevel.show_window_menu(&press.event) {
                return;
            }
        }
        let menu = gio::Menu::new();
        menu.append(Some(&gettext("Mi_nimize")), Some("window.minimize"));
        menu.append(
            Some(&if window.is_maximized() {
                gettext("_Restore")
            } else {
                gettext("Ma_ximize")
            }),
            Some("window.toggle-maximized"),
        );
        let close = gio::Menu::new();
        close.append(Some(&gettext("_Close")), Some("window.close"));
        menu.append_section(None, &close);
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(&self.web);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(
            press.x as i32,
            press.y as i32,
            1,
            1,
        )));
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    fn on_header_style(&self, message: &Value) {
        self.header.replace(Some(CssHeader {
            colors: HeaderColors {
                primary: parse_color(&message["primary"]),
                text: parse_color(&message["text"]),
                background: parse_color(&message["background"]),
                fill: parse_color(&message["fill"]),
                on_fill: parse_color(&message["onFill"]),
            },
            height: message["height"].as_f64().unwrap_or(56.0),
        }));
        if let Some(width) = message["width"].as_f64() {
            let before = self.scale.get();
            self.update_scale(width);
            if self.scale.get() != before {
                return;
            }
        }
        self.emit_header();
    }

    fn emit_header(&self) {
        let Some(header) = *self.header.borrow() else {
            return;
        };
        let style = HeaderStyle {
            colors: header.colors,
            height: (header.height * self.scale()).round() as i32,
        };
        log::debug!("Header style {style:?}");
        if let Some(f) = self.header_style.borrow().as_ref() {
            f(Some(style));
        }
    }

    /// A full load shows the frontend's launch screen until its toolbar is
    /// drawn again.
    fn on_load_started(&self) {
        if let Some(f) = self.default_dashboard_handler.borrow().as_ref() {
            f(false);
        }
        if let Some(f) = self.page_menu_handler.borrow().as_ref() {
            f(None);
        }
        if self.toolbar.take().is_some() {
            self.emit_toolbar();
        }
        if self.header.take().is_some()
            && let Some(f) = self.header_style.borrow().as_ref()
        {
            f(None);
        }
    }

    /// Opens a server path such as `/lovelace/0`; an empty path is the
    /// frontend's default dashboard.
    pub fn open(&self, path: &str) {
        let path = format!("/{}", path.trim_start_matches('/'));
        self.path.replace(path.clone());
        let base = self.base.borrow().clone();
        if base.is_empty() {
            self.show_error(&gettext("Not connected to Home Assistant"));
            return;
        }
        self.stack.set_visible_child_name("web");
        self.web.load_uri(&start_url(&base, &path));
    }

    pub fn is_open(&self) -> bool {
        !self.path.borrow().is_empty()
    }

    /// Leaves whatever page or app panel is showing for the default dashboard
    /// without reloading the frontend.
    pub fn go_home(&self) {
        if self.is_open() && self.stack.visible_child_name().as_deref() == Some("web") {
            self.run("window.sanctumGoHome?.(); undefined;".to_owned());
        } else {
            self.open("/");
        }
    }

    pub fn reload(&self) {
        let path = self.path.borrow().clone();
        self.open(&path);
    }

    /// WebKit applies a new policy to the pages it draws after the change, so
    /// the page is reloaded.
    pub fn apply_rendering(&self) {
        if let Some(settings) = webkit::prelude::WebViewExt::settings(&self.web) {
            apply_rendering(&settings);
        }
        self.update_user_content();
        if self.is_open() {
            self.reload();
        }
    }

    /// The internal and external URLs differ, so the page reloads on a switch.
    pub fn set_base(&self, base: &str) {
        if *self.base.borrow() == base {
            return;
        }
        self.base.replace(base.to_owned());
        if !self.path.borrow().is_empty() {
            self.reload();
        }
    }

    fn remember_path(&self, uri: &str) {
        let base = self.base.borrow();
        if origin(uri) != origin(&base) {
            return;
        }
        if let Ok(parsed) = glib::Uri::parse(uri, glib::UriFlags::NONE) {
            let query = parsed
                .query()
                .map(|query| {
                    query
                        .split('&')
                        .filter(|part| *part != "external_auth=1")
                        .collect::<Vec<_>>()
                        .join("&")
                })
                .filter(|query| !query.is_empty());
            let path = match query {
                Some(query) => format!("{}?{query}", parsed.path()),
                None => parsed.path().to_string(),
            };
            let _ = crate::ui::app().server().set_string("last-path", &path);
            self.path.replace(path);
        }
    }

    fn show_error(&self, detail: &str) {
        self.error
            .set_description(Some(&glib::markup_escape_text(detail)));
        self.stack.set_visible_child_name("error");
    }

    /// Pages on the server stay in the view; anything else opens in the
    /// browser.
    fn decide(&self, decision: &webkit::PolicyDecision, kind: webkit::PolicyDecisionType) -> bool {
        let Some(navigation) = decision.downcast_ref::<webkit::NavigationPolicyDecision>() else {
            return false;
        };
        let Some(uri) = navigation
            .navigation_action()
            .and_then(|action| action.request())
            .and_then(|request| request.uri())
        else {
            return false;
        };
        let external = kind == webkit::PolicyDecisionType::NewWindowAction
            || (uri.starts_with("http") && origin(&uri) != origin(&self.base.borrow()));
        if external {
            decision.ignore();
            launch(&uri);
            return true;
        }
        false
    }

    fn run(&self, script: String) {
        self.web
            .evaluate_javascript(&script, None, None, gio::Cancellable::NONE, |result| {
                if let Err(err) = result {
                    log::info!("Dashboard script failed: {err}");
                }
            });
    }

    fn on_get_auth(self: &Rc<Self>, message: Value) {
        let Some(callback) = message["callback"].as_str().and_then(identifier) else {
            return;
        };
        let callback = callback.to_owned();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let app = crate::ui::app();
            // A page can ask while the stored sign-in is still being read or
            // saved; answering "no" then shows the frontend's retry screen.
            let started = std::time::Instant::now();
            while app.client().credentials().is_none()
                && (app.is_loading() || app.is_signed_in())
                && started.elapsed() < AUTH_WAIT
            {
                glib::timeout_future(std::time::Duration::from_millis(200)).await;
            }
            if started.elapsed() >= std::time::Duration::from_millis(200) {
                log::debug!(
                    "Dashboard token waited {:?} for the sign-in",
                    started.elapsed()
                );
            }
            let result = app.client().fresh_token().await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            let script = match result {
                Ok((token, expires_in)) => format!(
                    "window.{callback}(true, {});",
                    json!({ "access_token": token, "expires_in": expires_in })
                ),
                Err(err) => {
                    log::warn!("No token for the dashboard: {err}");
                    format!("window.{callback}(false);")
                }
            };
            this.run(script);
        });
    }

    /// The frontend's Log Out button; Sanctum signs out after confirming.
    fn on_revoke_auth(self: &Rc<Self>, message: Value) {
        if let Some(callback) = message["callback"].as_str().and_then(identifier) {
            self.run(format!("window.{callback}(true);"));
        }
        let page = self.widget.clone();
        glib::spawn_future_local(async move {
            let dialog = adw::AlertDialog::builder()
                .heading(gettext("Sign Out?"))
                .body(gettext(
                    "Sanctum will forget this server and stop reporting sensors until you sign in again.",
                ))
                .close_response("cancel")
                .default_response("cancel")
                .build();
            dialog.add_responses(&[
                ("cancel", &gettext("_Cancel")),
                ("sign-out", &gettext("_Sign Out")),
            ]);
            dialog.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
            if dialog.choose_future(Some(&page)).await == "sign-out" {
                crate::ui::app().sign_out().await;
            }
        });
    }

    /// Sends the frontend a command, as the official apps do.
    fn send_command(&self, command: &str, payload: Value) {
        let message = json!({ "id": 0, "type": "command", "command": command, "payload": payload });
        self.run(format!("window.externalBus({message});"));
    }

    fn open_scanner(self: &Rc<Self>, payload: &Value) {
        if let Some(previous) = self.scanner.take() {
            previous.close();
        }
        let text = |key: &str| payload[key].as_str().unwrap_or_default().to_owned();
        let request = super::scanner::Request {
            title: text("title"),
            description: text("description"),
            alternative: payload["alternative_option_label"]
                .as_str()
                .map(str::to_owned),
        };
        let weak = Rc::downgrade(self);
        let scanner = super::scanner::Scanner::open(&self.widget, request, move |outcome| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            match outcome {
                super::scanner::Outcome::Found { raw, format } => this.send_command(
                    "bar_code/scan_result",
                    json!({ "rawValue": raw, "format": format }),
                ),
                super::scanner::Outcome::Canceled => {
                    this.scanner.take();
                    this.send_command("bar_code/aborted", json!({ "reason": "canceled" }));
                }
                super::scanner::Outcome::Alternative => {
                    this.scanner.take();
                    this.send_command(
                        "bar_code/aborted",
                        json!({ "reason": "alternative_options" }),
                    );
                }
            }
        });
        self.scanner.replace(Some(scanner));
    }

    fn on_bus(self: &Rc<Self>, message: Value) {
        let id = message["id"].clone();
        match message["type"].as_str().unwrap_or_default() {
            "config/get" => {
                let reply = json!({
                    "id": id,
                    "type": "result",
                    "success": true,
                    "result": {
                        "hasSettingsScreen": true,
                        "hasAssist": true,
                        "canWriteTag": false,
                        "hasExoPlayer": false,
                        "canCommissionMatter": false,
                        "canImportThreadCredentials": false,
                        "hasBarCodeScanner": 1,
                        "canSetupImprov": false,
                        "downloadFileSupported": false,
                        "appVersion": crate::config::VERSION,
                    },
                });
                self.run(format!("window.externalBus({reply});"));
            }
            "config_screen/show" => {
                crate::ui::app().activate_action("preferences", None);
            }
            "assist/show" => crate::ui::app().present_assist(false),
            // Every haptic type maps to one short event; feedbackd themes
            // have no finer touch patterns.
            "haptic" => {
                glib::spawn_future_local(crate::control::ringer::trigger("button-pressed"));
            }
            "theme-update" => self.run(HEADER_STYLE.to_owned()),
            "bar_code/scan" => self.open_scanner(&message["payload"]),
            "bar_code/notify" => {
                if let Some(scanner) = self.scanner.borrow().as_ref() {
                    scanner.notify(message["payload"]["message"].as_str().unwrap_or_default());
                }
            }
            "bar_code/close" => {
                if let Some(scanner) = self.scanner.take() {
                    scanner.close();
                }
            }
            "connection-status" => log::debug!(
                "Dashboard connection: {}",
                message["payload"]["event"].as_str().unwrap_or_default()
            ),
            other => log::debug!("Dashboard message {other} ignored"),
        }
    }
}

/// Message bodies arrive as JavaScript objects; JSON is the simplest bridge.
fn parse(value: &webkit::javascriptcore::Value) -> Value {
    value
        .to_json(0)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

pub fn launch(uri: &str) {
    gtk::UriLauncher::new(uri).launch(
        crate::ui::app().active_window().as_ref(),
        gio::Cancellable::NONE,
        |result| {
            if let Err(err) = result {
                log::warn!("Could not open link: {err}");
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_urls() {
        assert_eq!(
            start_url("https://ha.example:8123/", "lovelace"),
            "https://ha.example:8123/lovelace?external_auth=1"
        );
        assert_eq!(
            start_url("http://ha", "/history?entity_id=light.a"),
            "http://ha/history?entity_id=light.a&external_auth=1"
        );
        assert_eq!(
            identifier("externalAuthSetToken"),
            Some("externalAuthSetToken")
        );
        assert_eq!(identifier("alert(1)//"), None);
    }
}
