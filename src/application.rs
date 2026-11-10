use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib};

use crate::agent::Agent;
use crate::config::{APP_ID, VERSION};
use crate::ha;
use crate::model::Store;
use crate::notify::{self, Click, Notifier};
use crate::reregister::Reregistration;
use crate::server_settings::ServerSettings;
use crate::ui::{AssistWindow, PreferencesDialog, Window};

/// The ID of the notification sent when automatic registration gives up.
const REGISTRATION_NOTIFICATION: &str = "registration";

mod imp {
    use super::*;

    pub struct Application {
        pub settings: gio::Settings,
        pub server: std::rc::Rc<ServerSettings>,
        pub client: ha::Client,
        pub store: Store,
        pub agent: Agent,
        pub notifier: std::cell::OnceCell<std::rc::Rc<Notifier>>,
        pub reregistration: Reregistration,
        pub window: glib::WeakRef<Window>,
        pub assist_window: glib::WeakRef<AssistWindow>,
        pub background_hold: RefCell<Option<gio::ApplicationHoldGuard>>,
        /// Closes a hidden main window, which frees the page.
        pub unload_timer: RefCell<Option<glib::SourceId>>,
        pub started: Cell<bool>,
        /// True while stored credentials are being read at startup.
        pub loading: Cell<bool>,
    }

    impl Default for Application {
        fn default() -> Self {
            let client = ha::Client::default();
            let store = Store::new(&client);
            let settings = gio::Settings::new(APP_ID);
            let server = ServerSettings::new(&settings);
            let agent = Agent::new(&client, &settings, &server);
            Self {
                settings,
                server,
                client,
                store,
                agent,
                notifier: Default::default(),
                reregistration: Default::default(),
                window: Default::default(),
                assist_window: Default::default(),
                background_hold: Default::default(),
                unload_timer: Default::default(),
                started: Default::default(),
                loading: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Application {
        const NAME: &'static str = "SanctumApplication";
        type Type = super::Application;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for Application {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_main_option(
                "assist",
                glib::Char::from(b'a'),
                glib::OptionFlags::NONE,
                glib::OptionArg::None,
                &gettext("Open the Assist window"),
                None,
            );
            obj.add_main_option(
                "voice",
                glib::Char::from(0u8),
                glib::OptionFlags::NONE,
                glib::OptionArg::None,
                &gettext("Start listening when used with --assist"),
                None,
            );
            obj.add_main_option(
                "background",
                glib::Char::from(b'b'),
                glib::OptionFlags::NONE,
                glib::OptionArg::None,
                &gettext("Start without opening a window"),
                None,
            );
        }
    }

    impl ApplicationImpl for Application {
        fn startup(&self) {
            self.parent_startup();
            let obj = self.obj();
            let _ = self.notifier.set(Notifier::new(&*obj, &self.client));
            gtk::Window::set_default_icon_name(APP_ID);
            obj.setup_actions();
            obj.setup_settings();
            obj.setup_accent();
            // WebKit reads the color scheme again only when the GTK theme
            // name changes, and libadwaita's switch does not count as one.
            adw::StyleManager::default().connect_dark_notify(|_| {
                glib::idle_add_local_once(|| {
                    if let Some(settings) = gtk::Settings::default() {
                        settings.notify("gtk-theme-name");
                    }
                });
            });
            ha::trust::set_trusted(
                obj.settings()
                    .strv("trusted-certificates")
                    .iter()
                    .map(ToString::to_string),
            );
        }

        fn activate(&self) {
            self.parent_activate();
            let obj = self.obj();
            obj.start();
            obj.present_main_window();
        }

        fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
            let options = command_line.options_dict();
            let obj = self.obj();
            if options.contains("assist") {
                obj.start();
                obj.present_assist(options.contains("voice"));
            } else if options.contains("background") {
                obj.start();
                obj.start_services();
            } else {
                obj.activate();
            }
            glib::ExitCode::SUCCESS
        }
    }

    impl GtkApplicationImpl for Application {}
    impl AdwApplicationImpl for Application {}
}

glib::wrapper! {
    pub struct Application(ObjectSubclass<imp::Application>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Application {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", APP_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_COMMAND_LINE)
            .property("resource-base-path", "/net/dunevent/Sanctum/")
            .build()
    }

    pub fn settings(&self) -> &gio::Settings {
        &self.imp().settings
    }

    /// The signed-in server's settings.
    pub fn server(&self) -> gio::Settings {
        self.imp().server.get()
    }

    pub fn server_settings(&self) -> &std::rc::Rc<ServerSettings> {
        &self.imp().server
    }

    pub fn client(&self) -> &ha::Client {
        &self.imp().client
    }

    pub fn agent(&self) -> &Agent {
        &self.imp().agent
    }

    /// Sensors and other background work. Deferred so a cold `--assist` stays
    /// fast; starts with the main window or when running in the background.
    fn start_services(&self) {
        if self.is_signed_in() && self.is_companion() {
            self.agent().start();
            self.notifier().start();
        }
    }

    pub fn is_companion(&self) -> bool {
        self.server().string("app-mode") == "companion"
    }

    pub async fn use_as_companion(&self, device_name: &str) -> ha::Result<()> {
        let registration = self.request_registration(device_name).await?;
        self.clear_registration_alert();
        self.client().replace_registration(registration).await;
        let server = self.server();
        let _ = server.set_string("device-name", device_name);
        let _ = server.set_string("app-mode", "companion");
        self.sync_endpoints();
        self.start_services();
        Ok(())
    }

    async fn request_registration(
        &self,
        device_name: &str,
    ) -> ha::Result<ha::credentials::Registration> {
        let client = self.client();
        let credentials = client.credentials().ok_or(ha::Error::NotConnected)?;
        let base = ha::ServerUrl::parse(&client.active_url()).unwrap_or(credentials.server);
        let token = client.fresh_access_token().await?;
        let device = ha::device::DeviceInfo::detect(device_name);
        ha::registration::register(&base, &token, &credentials.device_id, &device).await
    }

    /// For a webhook or notification subscription that Home Assistant no
    /// longer recognizes.
    pub fn registration_lost(&self) {
        if !self.is_signed_in() || !self.is_companion() {
            return;
        }
        let Some(generation) = self.imp().reregistration.begin() else {
            return;
        };
        log::warn!("Home Assistant no longer has this device's registration; registering again");
        let app = self.clone();
        glib::spawn_future_local(async move {
            let client = app.client().clone();
            let name = app.server().string("device-name");
            let registration = app
                .imp()
                .reregistration
                .run(generation, &client, || app.request_registration(&name))
                .await;
            match registration {
                Some(registration) => app.apply_registration(registration).await,
                None if app.imp().reregistration.gave_up() => app.show_registration_alert(),
                None => {}
            }
        });
    }

    /// One attempt for the Register Again button, with errors shown as a
    /// toast.
    fn register_again(&self) {
        self.present_main_window();
        let app = self.clone();
        glib::spawn_future_local(async move {
            let name = app.server().string("device-name");
            match app.request_registration(&name).await {
                Ok(registration) => {
                    app.clear_registration_alert();
                    app.apply_registration(registration).await;
                }
                Err(err) => {
                    log::warn!("Could not register the device again: {err}");
                    if let Some(window) = app.imp().window.upgrade() {
                        window.show_toast(&crate::ui::error_text(&err));
                    }
                }
            }
        });
    }

    async fn apply_registration(&self, registration: ha::credentials::Registration) {
        self.client().replace_registration(registration).await;
        self.agent().resend_all();
        self.notifier().resubscribe();
    }

    pub fn registration_alert(&self) -> bool {
        self.imp().reregistration.gave_up()
    }

    fn show_registration_alert(&self) {
        let window = self.imp().window.upgrade();
        if let Some(window) = &window {
            window.set_registration_alert(true);
        }
        if window.is_some_and(|window| window.is_visible()) {
            return;
        }
        let notification = gio::Notification::new(&gettext("Not Registered in Home Assistant"));
        notification.set_body(Some(&gettext(
            "Notifications and sensor updates are off until this device is registered again.",
        )));
        notification.add_button(&gettext("Register Again"), "app.register-again");
        self.send_notification(Some(REGISTRATION_NOTIFICATION), &notification);
    }

    /// Also stops automatic attempts.
    fn clear_registration_alert(&self) {
        self.imp().reregistration.cancel();
        self.withdraw_notification(REGISTRATION_NOTIFICATION);
        if let Some(window) = self.imp().window.upgrade() {
            window.set_registration_alert(false);
        }
    }

    /// With `remove`, the device is deleted from Home Assistant, which needs
    /// an administrator.
    pub async fn use_as_frontend(&self, remove: bool) -> ha::Result<()> {
        self.clear_registration_alert();
        self.agent().pause();
        self.notifier().pause();
        let client = self.client().clone();
        let removed = if remove {
            match client.credentials() {
                Some(credentials) => {
                    ha::registration::remove(&client, &credentials.device_id).await
                }
                None => Ok(false),
            }
        } else {
            Ok(false)
        };
        client.set_registration(None).await;
        let _ = self.server().set_string("app-mode", "frontend");
        self.sync_endpoints();
        removed.map(drop)
    }

    fn notifier(&self) -> &std::rc::Rc<Notifier> {
        self.imp().notifier.get().expect("created in startup")
    }

    /// Notification clicks: `entityId:` targets open details, links to the
    /// server open in the dashboard view, and other URLs in the browser.
    fn open_click(&self, target: &str) {
        self.start();
        match Click::parse(Some(target)) {
            Click::Nothing => {}
            Click::App => self.present_main_window(),
            Click::Entity(entity_id) => {
                self.present_main_window();
                self.show_entity_when_loaded(entity_id);
            }
            Click::Url(url) => {
                if let Some(path) = self.server_path(&url) {
                    self.present_main_window();
                    if let Some(window) = self.imp().window.upgrade() {
                        window.show_web_path(&path);
                    }
                    return;
                }
                let url = if url.starts_with('/') {
                    let base = self.client().active_url();
                    let base = if base.is_empty() {
                        self.server().string("url").to_string()
                    } else {
                        base
                    };
                    format!("{}{url}", base.trim_end_matches('/'))
                } else {
                    url
                };
                gtk::UriLauncher::new(&url).launch(
                    self.active_window().as_ref(),
                    gio::Cancellable::NONE,
                    |result| {
                        if let Err(err) = result {
                            log::warn!("Could not open link: {err}");
                        }
                    },
                );
            }
        }
    }

    /// The path of a link to the connected server, which opens in the
    /// dashboard view. A bare path such as `/lovelace/cameras` counts.
    fn server_path(&self, url: &str) -> Option<String> {
        if url.starts_with('/') {
            return Some(url.to_owned());
        }
        let link = glib::Uri::parse(url, glib::UriFlags::NONE).ok()?;
        let server = glib::Uri::parse(&self.client().active_url(), glib::UriFlags::NONE).ok()?;
        if ha::trust::host_key(&link) != ha::trust::host_key(&server) {
            return None;
        }
        let path = link.path().to_string();
        Some(match link.query() {
            Some(query) => format!("{path}?{query}"),
            None => path,
        })
    }

    fn show_entity_when_loaded(&self, entity_id: String) {
        let show = |app: &Self, entity_id: &str| -> bool {
            let (Some(window), Some(entity)) =
                (app.imp().window.upgrade(), app.store().entity(entity_id))
            else {
                return false;
            };
            crate::ui::present_details(&window, &entity);
            true
        };
        if self.store().loaded() && show(self, &entity_id) {
            return;
        }
        let handler: std::rc::Rc<std::cell::RefCell<Option<glib::SignalHandlerId>>> =
            Default::default();
        let id = self.store().connect_loaded_notify(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[strong]
            handler,
            move |store| {
                if store.loaded() {
                    show(&app, &entity_id);
                    if let Some(id) = handler.take() {
                        store.disconnect(id);
                    }
                }
            }
        ));
        handler.replace(Some(id));
    }

    pub fn device_info(&self) -> ha::device::DeviceInfo {
        ha::device::DeviceInfo::detect(&self.server().string("device-name"))
    }

    pub fn store(&self) -> &Store {
        &self.imp().store
    }

    pub fn is_signed_in(&self) -> bool {
        !self.settings().string("server").is_empty() && !self.server().string("url").is_empty()
    }

    pub fn is_loading(&self) -> bool {
        self.imp().loading.get()
    }

    /// Loads stored credentials and connects. Builds no UI, so `--assist` and
    /// `--background` stay fast.
    fn start(&self) {
        let imp = self.imp();
        if imp.started.replace(true) {
            return;
        }
        self.update_background_hold();
        if self.settings().boolean("run-in-background") {
            self.sync_background_portal();
        }

        let url = self.server().string("url");
        if url.is_empty() {
            return;
        }
        let Ok(server) = ha::ServerUrl::parse(&url) else {
            log::warn!("Stored server address is invalid: {url}");
            self.forget_server();
            return;
        };

        imp.loading.set(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                let result = ha::credentials::load(&server).await;
                app.imp().loading.set(false);
                match result {
                    Ok(Some(credentials)) => app.connect_with(credentials),
                    Ok(None) => {
                        log::warn!("No stored credentials for {server}");
                        app.forget_server();
                    }
                    Err(err) => {
                        log::warn!("Could not read credentials: {err}");
                        app.forget_server();
                    }
                }
                if let Some(window) = app.imp().window.upgrade() {
                    window.update_page();
                }
            }
        ));
    }

    fn forget_server(&self) {
        self.imp().server.clear();
        let _ = self.settings().set_string("server", "");
    }

    pub fn present_main_window(&self) {
        let window = self.imp().window.upgrade().unwrap_or_else(|| {
            let window = Window::new(self);
            self.imp().window.set(Some(&window));
            window.connect_visible_notify(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |window| app.on_window_visibility(window)
            ));
            window
        });
        self.update_window_hiding();
        window.present();
        self.start_services();
    }

    /// A hidden window keeps the application alive, so the page is kept only
    /// while Sanctum runs in the background.
    fn update_window_hiding(&self) {
        let settings = self.settings();
        let keep = settings.boolean("keep-web-loaded") && settings.boolean("run-in-background");
        if let Some(window) = self.imp().window.upgrade() {
            window.set_hide_on_close(keep);
        }
    }

    /// A page kept loaded is released once the window has been hidden for the
    /// chosen time.
    fn on_window_visibility(&self, window: &Window) {
        if let Some(timer) = self.imp().unload_timer.take() {
            timer.remove();
        }
        let minutes = self.settings().uint("keep-web-loaded-minutes");
        if window.is_visible() || !window.hides_on_close() || minutes == 0 {
            return;
        }
        let timer = glib::timeout_add_seconds_local_once(
            minutes * 60,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                #[weak]
                window,
                move || {
                    app.imp().unload_timer.take();
                    if !window.is_visible() {
                        log::debug!("Releasing the page after {minutes} minutes hidden");
                        window.destroy();
                    }
                }
            ),
        );
        self.imp().unload_timer.replace(Some(timer));
    }

    /// `voice` listens even when the preference is off.
    pub fn present_assist(&self, voice: bool) {
        let window = self.imp().assist_window.upgrade().unwrap_or_else(|| {
            let window = AssistWindow::new(self);
            self.imp().assist_window.set(Some(&window));
            window
        });
        // Compositors give focus to newly mapped windows but refuse it to an
        // open window asking without an activation token, which the assistant
        // button cannot provide. Mapping the window again counts as new.
        if window.is_visible() && !window.is_active() {
            window.set_visible(false);
        }
        window.present();
        window.listen_on_open(voice);
    }

    /// An empty `device_name` keeps the stored one, as on a new sign-in to the
    /// same server.
    pub async fn complete_sign_in(
        &self,
        credentials: ha::Credentials,
        device_name: &str,
    ) -> ha::Result<()> {
        ha::credentials::store(&credentials).await?;
        let companion = credentials.registration.is_some();
        self.imp().server.sign_in(
            self.settings(),
            credentials.server.as_str(),
            device_name,
            companion,
        );
        self.connect_with(credentials);
        self.update_background_hold();
        self.start_services();
        Ok(())
    }

    /// Endpoints go first so the first attempt uses the right address.
    fn connect_with(&self, credentials: ha::Credentials) {
        self.client()
            .set_endpoints(self.endpoints(credentials.registration.as_ref()));
        self.client().set_credentials(Some(credentials));
    }

    fn endpoints(
        &self,
        registration: Option<&ha::credentials::Registration>,
    ) -> ha::endpoints::Endpoints {
        let settings = self.server();
        let url = |key: &str| ha::ServerUrl::parse(&settings.string(key)).ok();
        ha::endpoints::Endpoints {
            internal: url("internal-url"),
            external: url("external-url"),
            remote_ui: registration
                .and_then(|registration| registration.remote_ui_url.as_deref())
                .and_then(|value| ha::ServerUrl::parse(value).ok()),
            home_networks: settings
                .strv("home-networks")
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }

    fn sync_endpoints(&self) {
        let registration = self.client().credentials().and_then(|c| c.registration);
        self.client()
            .set_endpoints(self.endpoints(registration.as_ref()));
    }

    pub fn trust_certificate(&self, info: &ha::trust::CertificateInfo) {
        ha::trust::trust(info);
        let mut entries: Vec<String> = self
            .settings()
            .strv("trusted-certificates")
            .iter()
            .map(ToString::to_string)
            // A host keeps one pinned certificate; a newly accepted one replaces it.
            .filter(|entry| !entry.starts_with(&format!("{} ", info.host)))
            .collect();
        entries.push(info.entry());
        let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
        let _ = self.settings().set_strv("trusted-certificates", refs);
    }

    pub async fn sign_out(&self) {
        let client = self.client();
        self.clear_registration_alert();
        self.agent().pause();
        if let Some(credentials) = client.credentials() {
            client.set_credentials(None);
            ha::auth::revoke(&credentials.server, &credentials.auth).await;
            if let Err(err) = ha::credentials::clear(&credentials.server).await {
                log::warn!("Could not remove stored credentials: {err}");
            }
        }
        self.forget_server();
        self.update_background_hold();
    }

    fn update_background_hold(&self) {
        let imp = self.imp();
        let wanted = self.settings().boolean("run-in-background") && self.is_signed_in();
        let held = imp.background_hold.borrow().is_some();
        if wanted && !held {
            imp.background_hold.replace(Some(self.hold()));
            self.start_services();
        } else if !wanted && held {
            imp.background_hold.replace(None);
        }
    }

    fn sync_background_portal(&self) {
        let settings = self.settings();
        let background = settings.boolean("run-in-background");
        let autostart = background && settings.boolean("start-at-login");
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                let reason =
                    gettext("Keep the Home Assistant connection open for notifications and Assist");
                match crate::background::request(autostart, &reason).await {
                    Ok(granted) => {
                        if background && !granted.background {
                            app.show_toast(&gettext("Background permission was denied"));
                        } else if autostart && !granted.autostart {
                            app.show_toast(&gettext("Start at login was not allowed"));
                        }
                    }
                    Err(err) => log::warn!("Background portal request failed: {err}"),
                }
            }
        ));
    }

    pub fn show_toast(&self, message: &str) {
        if let Some(window) = self.imp().window.upgrade() {
            window.show_toast(message);
        }
    }

    fn setup_settings(&self) {
        let settings = self.settings();
        // Conversation mode needs spoken replies, so each follows the other.
        settings.connect_changed(Some("assist-conversation-mode"), |settings, key| {
            if settings.boolean(key) && !settings.boolean("assist-speak-replies") {
                let _ = settings.set_boolean("assist-speak-replies", true);
            }
        });
        settings.connect_changed(Some("assist-speak-replies"), |settings, key| {
            if !settings.boolean(key) && settings.boolean("assist-conversation-mode") {
                let _ = settings.set_boolean("assist-conversation-mode", false);
            }
        });
        for key in ["keep-web-loaded", "run-in-background"] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |_, _| app.update_window_hiding()
                ),
            );
        }
        for key in ["web-rendering", "web-reduce-motion", "web-show-fps"] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |_, key| {
                        app.apply_motion();
                        if let Some(window) = app.imp().window.upgrade() {
                            window.apply_rendering();
                        }
                        if key == "web-rendering" && crate::ui::rendering_needs_restart() {
                            app.show_toast(&gettext(
                                "Restart Sanctum to finish changing how pages are drawn",
                            ));
                        }
                    }
                ),
            );
        }
        settings.connect_changed(
            Some("swipe-navigation"),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_, _| {
                    if let Some(window) = app.imp().window.upgrade() {
                        window.apply_swipe_navigation();
                    }
                }
            ),
        );
        settings.connect_changed(
            Some("pull-to-refresh"),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_, _| {
                    if let Some(window) = app.imp().window.upgrade() {
                        window.update_page_settings();
                    }
                }
            ),
        );
        settings.connect_changed(
            Some("web-zoom"),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |settings, key| {
                    if let Some(window) = app.imp().window.upgrade() {
                        window.set_zoom(settings.double(key));
                    }
                }
            ),
        );
        settings.connect_changed(
            Some("run-in-background"),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_, _| {
                    app.update_background_hold();
                    app.sync_background_portal();
                }
            ),
        );
        settings.connect_changed(
            Some("start-at-login"),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_, _| app.sync_background_portal()
            ),
        );
        settings.connect_changed(
            Some("server"),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_, _| app.update_background_hold()
            ),
        );
        let weak = self.downgrade();
        self.imp().server.watch(
            &["internal-url", "external-url", "home-networks"],
            move |_| {
                if let Some(app) = weak.upgrade() {
                    app.sync_endpoints();
                }
            },
        );
        self.apply_motion();
    }

    fn apply_motion(&self) {
        crate::ui::rendering::apply_motion(self.settings().boolean("web-reduce-motion"));
    }

    /// Sanctum's accent follows the Home Assistant theme's primary color.
    fn setup_accent(&self) {
        let provider = gtk::CssProvider::new();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        let app_settings = self.settings().clone();
        let apply = move |server: &gio::Settings| {
            let color = if app_settings.boolean("server-accent") {
                server.string("accent-color")
            } else {
                "".into()
            };
            provider.load_from_string(&accent_css(&color));
        };
        apply(&self.server());
        let apply = Rc::new(apply);
        self.imp().server.watch(&["accent-color"], {
            let apply = apply.clone();
            move |server| apply(server)
        });
        let weak = self.downgrade();
        self.settings()
            .connect_changed(Some("server-accent"), move |_, _| {
                if let Some(app) = weak.upgrade() {
                    apply(&app.server());
                }
            });
    }

    fn setup_actions(&self) {
        let quit = gio::ActionEntry::builder("quit")
            .activate(|app: &Self, _, _| {
                app.imp().background_hold.replace(None);
                app.quit();
            })
            .build();
        let preferences = gio::ActionEntry::builder("preferences")
            .activate(|app: &Self, _, _| app.show_preferences())
            .build();
        let about = gio::ActionEntry::builder("about")
            .activate(|app: &Self, _, _| app.show_about())
            .build();
        let assist = gio::ActionEntry::builder("assist")
            .activate(|app: &Self, _, _| {
                app.start();
                app.present_assist(false);
            })
            .build();
        let assist_voice = gio::ActionEntry::builder("assist-voice")
            .activate(|app: &Self, _, _| {
                app.start();
                app.present_assist(true);
            })
            .build();
        let reconnect = gio::ActionEntry::builder("reconnect")
            .activate(|app: &Self, _, _| app.client().reconnect())
            .build();
        let register_again = gio::ActionEntry::builder("register-again")
            .activate(|app: &Self, _, _| {
                app.start();
                app.register_again();
            })
            .build();
        let open_click = gio::ActionEntry::builder("open-click")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|app: &Self, _, target| {
                if let Some(target) = target.and_then(|t| t.str()) {
                    app.open_click(target);
                }
            })
            .build();
        let notification_action = gio::ActionEntry::builder("notification-action")
            .parameter_type(Some(glib::VariantTy::new("(ssss)").expect("valid type")))
            .activate(|app: &Self, _, parameter| {
                let Some((action, tag, title, message)) =
                    parameter.and_then(|p| p.get::<(String, String, String, String)>())
                else {
                    return;
                };
                app.start();
                app.notifier()
                    .send_action(notify::action_event(&action, &tag, &title, &message));
            })
            .build();
        self.add_action_entries([
            quit,
            preferences,
            about,
            assist,
            assist_voice,
            reconnect,
            register_again,
            open_click,
            notification_action,
        ]);

        self.set_accels_for_action("app.quit", &["<Control>q"]);
        self.set_accels_for_action("app.preferences", &["<Control>comma"]);
        self.set_accels_for_action("win.reload", &["<Control>r", "F5"]);
        self.set_accels_for_action("win.overview", &["<Alt>Home"]);
        self.set_accels_for_action(
            "win.zoom-in",
            &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
        );
        self.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
        self.set_accels_for_action("win.zoom-reset", &["<Control>0", "<Control>KP_0"]);
        self.set_accels_for_action("window.close", &["<Control>w"]);
    }

    fn show_preferences(&self) {
        self.present_main_window();
        if let Some(window) = self.imp().window.upgrade() {
            PreferencesDialog::new(self).present(Some(&window));
        }
    }

    fn show_about(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name(gettext("Sanctum"))
            .application_icon(APP_ID)
            .developer_name("Sanctum contributors")
            .version(VERSION)
            .website("https://dev.dunevent.net/aj/Sanctum")
            .issue_url("https://dev.dunevent.net/aj/Sanctum/issues")
            .license_type(gtk::License::Gpl30)
            .developers(["AJ Dunevent"])
            .translator_credits(gettext("translator-credits"))
            .build();
        dialog.add_link(&gettext("Donate"), "https://dunevent.net/donate");
        if let Some(window) = self.active_window() {
            dialog.present(Some(&window));
        }
    }
}

impl Default for Application {
    fn default() -> Self {
        Self::new()
    }
}

/// Text on the accent is white or near black, whichever reads better.
fn accent_css(value: &str) -> String {
    let Ok(color) = gdk::RGBA::parse(value) else {
        return String::new();
    };
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(color.red())
        + 0.7152 * linear(color.green())
        + 0.0722 * linear(color.blue());
    let foreground = if luminance > 0.4 {
        "rgb(0 0 6 / 80%)"
    } else {
        "white"
    };
    format!(":root {{ --accent-bg-color: {color}; --accent-fg-color: {foreground}; }}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_accent_text() {
        assert!(accent_css("#03a9f4").contains("white"));
        assert!(accent_css("#ffeb3b").contains("80%"));
        assert_eq!(accent_css(""), "");
    }
}
