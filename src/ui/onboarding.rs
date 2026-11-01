use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use futures_channel::oneshot;
use futures_util::future::{Either, select};
use gettextrs::gettext;
use gtk::glib;
use webkit::prelude::*;

use super::{app, confirm_trust, error_text};
use crate::ha::credentials::Registration;
use crate::ha::discovery;
use crate::ha::{self, Credentials};

/// The login page names only the official Android and iOS apps; any other
/// client shows its client ID URL as a warning. Sanctum's view shows its
/// name as an ordinary notice. The frontend may render again, so the check
/// repeats.
const CLIENT_NAME_SCRIPT: &str = r#"setInterval(() => {
  const page = document.querySelector("ha-authorize");
  for (const alert of (page?.shadowRoot ?? page)?.querySelectorAll("ha-alert") ?? []) {
    const name = alert.querySelector("b");
    if (name && name.textContent.trim() === CLIENT_ID) {
      name.textContent = "Sanctum";
      alert.alertType = "info";
    }
  }
}, 200);"#;

#[derive(Debug, Clone)]
pub struct DeviceSlot {
    server: ha::ServerUrl,
    device_id: String,
    registration: Option<Registration>,
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/net/dunevent/Sanctum/ui/onboarding.ui")]
    pub struct Onboarding {
        #[template_child]
        pub navigation: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub url_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub continue_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub found_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub found_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub login_holder: TemplateChild<adw::Bin>,
        #[template_child]
        pub token_row: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub token_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub name_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub intro_sensors_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub intro_controls_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub intro_background_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub name_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub intro_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub internal_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub external_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub network_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub home_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub connection_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub background_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub login_row: TemplateChild<adw::SwitchRow>,

        pub server: RefCell<Option<ha::ServerUrl>>,
        /// Reused on re-sign-in and on retries, so Home Assistant sees one device.
        pub device: RefCell<Option<DeviceSlot>>,
        /// The name chosen for a new device.
        pub device_name: RefCell<String>,
        /// Sign-in that waits for the mode choice and device name.
        pub pending: RefCell<Option<(ha::ServerUrl, ha::Auth)>>,
        pub login: RefCell<Option<Rc<ha::auth::WebFlow>>>,
        pub authorize_url: RefCell<String>,
        pub cancel_browser: RefCell<Option<oneshot::Sender<()>>>,
        /// The welcome page stays up after sign-in until it is dismissed.
        pub presenting: Cell<bool>,
        pub browser: RefCell<Option<discovery::Browser>>,
        /// Servers announced on the local network, in list order.
        pub found: RefCell<Vec<discovery::Found>>,
        /// What the home network switch adds.
        pub current_networks: RefCell<Vec<String>>,
    }

    impl std::fmt::Debug for Onboarding {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Onboarding").finish_non_exhaustive()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Onboarding {
        const NAME: &'static str = "SanctumOnboarding";
        type Type = super::Onboarding;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_instance_callbacks();
            klass.install_action("onboarding.browser", None, |obj, _, _| {
                obj.start_browser();
            });
            klass.install_action("onboarding.token", None, |obj, _, _| {
                let imp = obj.imp();
                imp.navigation.push_by_tag("token");
                imp.token_row.grab_focus();
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Onboarding {
        fn constructed(&self) {
            self.parent_constructed();
            self.intro_icon.set_icon_name(Some(crate::config::APP_ID));
            let app = app();
            let settings = app.settings();
            settings
                .bind("run-in-background", &*self.background_row, "active")
                .build();
            settings
                .bind("start-at-login", &*self.login_row, "active")
                .build();
            self.background_row
                .bind_property("active", &*self.login_row, "sensitive")
                .sync_create()
                .build();
            self.navigation.connect_popped(glib::clone!(
                #[weak(rename_to = imp)]
                self,
                move |_, page| match page.tag().as_deref() {
                    Some("browser") => {
                        if let Some(cancel) = imp.cancel_browser.take() {
                            let _ = cancel.send(());
                        }
                    }
                    Some("login") => imp.obj().stop_login(),
                    _ => {}
                }
            ));
        }
    }

    impl WidgetImpl for Onboarding {}
    impl BinImpl for Onboarding {}
}

glib::wrapper! {
    pub struct Onboarding(ObjectSubclass<imp::Onboarding>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

#[gtk::template_callbacks]
impl Onboarding {
    pub fn reset(&self) {
        let imp = self.imp();
        imp.device.replace(None);
        imp.server.replace(None);
        imp.pending.replace(None);
        imp.device_name.replace(String::new());
        imp.presenting.set(false);
        imp.token_row.set_text("");
        imp.navigation.pop_to_tag("welcome");
        self.stop_login();
        self.set_busy(false);
        self.start_discovery();
    }

    pub fn reauthenticate(&self, credentials: Credentials) {
        let imp = self.imp();
        imp.url_row.set_text(credentials.server.as_str());
        imp.server.replace(Some(credentials.server.clone()));
        imp.device.replace(Some(DeviceSlot {
            server: credentials.server,
            device_id: credentials.device_id,
            registration: credentials.registration,
        }));
        imp.token_row.set_text("");
        imp.navigation.pop_to_tag("welcome");
        self.set_busy(false);
        self.start_login();
    }

    /// True while the welcome page is shown after a first sign-in.
    pub fn is_presenting(&self) -> bool {
        self.imp().presenting.get()
    }

    fn set_busy(&self, busy: bool) {
        let imp = self.imp();
        imp.continue_button.set_sensitive(!busy);
        imp.url_row.set_sensitive(!busy);
        imp.token_button.set_sensitive(!busy);
        imp.name_button.set_sensitive(!busy);
    }

    fn toast(&self, message: &str) {
        if let Some(window) = self.root().and_downcast::<super::Window>() {
            window.show_toast(message);
        }
    }

    fn start_discovery(&self) {
        let imp = self.imp();
        imp.browser.replace(None);
        imp.found.replace(Vec::new());
        imp.found_list.remove_all();
        imp.found_box.set_visible(false);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let weak = obj.downgrade();
                let browser = discovery::browse(move |found| {
                    if let Some(obj) = weak.upgrade() {
                        obj.add_found(found);
                    }
                })
                .await;
                obj.imp().browser.replace(browser);
            }
        ));
    }

    fn add_found(&self, found: discovery::Found) {
        let imp = self.imp();
        if imp
            .found
            .borrow()
            .iter()
            .any(|known| known.internal_url == found.internal_url)
        {
            return;
        }
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&found.name))
            .subtitle(glib::markup_escape_text(&found.internal_url))
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        imp.found_list.append(&row);
        imp.found.borrow_mut().push(found);
        imp.found_box.set_visible(true);
    }

    #[template_callback]
    fn on_found_activated(&self, row: &gtk::ListBoxRow) {
        let imp = self.imp();
        let Some(found) = usize::try_from(row.index())
            .ok()
            .and_then(|index| imp.found.borrow().get(index).cloned())
        else {
            return;
        };
        imp.url_row.set_text(&found.internal_url);
        self.on_continue_clicked();
    }

    #[template_callback]
    fn on_url_activated(&self) {
        self.on_continue_clicked();
    }

    #[template_callback]
    fn on_continue_clicked(&self) {
        let imp = self.imp();
        let candidates = match ha::ServerUrl::candidates(&imp.url_row.text()) {
            Ok(candidates) => candidates,
            Err(err) => {
                self.toast(&error_text(&err));
                return;
            }
        };
        self.set_busy(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let mut last_error = None;
                for (server, asked) in candidates {
                    // A filtered port would otherwise hold the plain HTTP try
                    // back for the whole session timeout.
                    let result = glib::future_with_timeout(
                        std::time::Duration::from_secs(8),
                        ha::auth::probe(&server),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Err(ha::Error::Network(format!("{server} did not answer")))
                    });
                    match result {
                        Ok(()) => {
                            obj.set_busy(false);
                            let imp = obj.imp();
                            imp.url_row.set_text(server.as_str());
                            imp.server.replace(Some(server));
                            obj.start_login();
                            return;
                        }
                        // Typing http:// asks for no certificate decisions.
                        Err(ha::Error::UntrustedCertificate(info)) if asked => {
                            obj.set_busy(false);
                            if confirm_trust(&obj, &info).await {
                                app().trust_certificate(&info);
                                obj.on_continue_clicked();
                                return;
                            }
                            obj.set_busy(true);
                            last_error = Some(ha::Error::UntrustedCertificate(info));
                        }
                        Err(err) => {
                            log::debug!("{server} not usable: {err}");
                            last_error = Some(err);
                        }
                    }
                }
                obj.set_busy(false);
                if let Some(err) = last_error {
                    obj.toast(&error_text(&err));
                }
            }
        ));
    }

    /// Shows the server's login page. The view is created once per visit and
    /// nothing reloads it, so typing is never interrupted.
    fn start_login(&self) {
        let imp = self.imp();
        let Some(server) = imp.server.borrow().clone() else {
            return;
        };
        let flow = Rc::new(ha::auth::WebFlow::start(&server));
        let session = webkit::NetworkSession::new_ephemeral();
        let web = webkit::WebView::builder()
            .network_session(&session)
            .vexpand(true)
            .build();
        super::dashboard::apply_settings(&web);
        if let Some(content) = web.user_content_manager() {
            super::dashboard::add_compatibility_scripts(&content);
            content.add_script(&webkit::UserScript::new(
                &CLIENT_NAME_SCRIPT.replace(
                    "CLIENT_ID",
                    &serde_json::to_string(ha::auth::CLIENT_ID).expect("a string"),
                ),
                webkit::UserContentInjectedFrames::TopFrame,
                webkit::UserScriptInjectionTime::End,
                &[],
                &[],
            ));
        }

        let progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Start)
            .css_classes(["osd"])
            .build();
        web.connect_estimated_load_progress_notify(glib::clone!(
            #[weak]
            progress,
            move |web| {
                let fraction = web.estimated_load_progress();
                progress.set_fraction(fraction);
                progress.set_visible(fraction < 1.0);
            }
        ));
        web.connect_decide_policy(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[strong]
            flow,
            #[upgrade_or]
            false,
            move |_, decision, kind| obj.decide_login(&flow, decision, kind)
        ));
        web.connect_create(|_, action| {
            if let Some(uri) = action.clone().request().and_then(|r| r.uri()) {
                super::dashboard::launch(&uri);
            }
            None
        });
        web.connect_load_failed_with_tls_errors(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[upgrade_or]
            false,
            move |web, uri, certificate, errors| {
                let Ok(parsed) = glib::Uri::parse(uri, glib::UriFlags::NONE) else {
                    return false;
                };
                let info = ha::trust::describe(ha::trust::host_key(&parsed), certificate, errors);
                if ha::trust::is_trusted(&info) {
                    if let Some(session) = web.network_session() {
                        session.allow_tls_certificate_for_host(
                            certificate,
                            &parsed.host().unwrap_or_default(),
                        );
                    }
                    web.load_uri(uri);
                } else {
                    obj.login_failed(&gettext("The server's certificate is not trusted"));
                }
                true
            }
        ));
        web.connect_load_failed(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[upgrade_or]
            false,
            move |_, _, _, error| {
                if error.matches(webkit::NetworkError::Cancelled)
                    || error.matches(webkit::PolicyError::FrameLoadInterruptedByPolicyChange)
                {
                    return false;
                }
                obj.login_failed(error.message());
                true
            }
        ));

        let overlay = gtk::Overlay::builder().child(&web).build();
        overlay.add_overlay(&progress);
        imp.login_holder.set_child(Some(&overlay));
        web.load_uri(flow.authorize_url());
        imp.login.replace(Some(flow));
        if imp.navigation.visible_page_tag().as_deref() != Some("login") {
            imp.navigation.push_by_tag("login");
        }
    }

    fn stop_login(&self) {
        let imp = self.imp();
        imp.login.replace(None);
        imp.login_holder.set_child(gtk::Widget::NONE);
    }

    fn login_failed(&self, message: &str) {
        let imp = self.imp();
        let page = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title(gettext("Login Page Unavailable"))
            .description(glib::markup_escape_text(message))
            .build();
        let retry = gtk::Button::builder()
            .label(gettext("_Retry"))
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        retry.connect_clicked(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| obj.start_login()
        ));
        page.set_child(Some(&retry));
        imp.login.replace(None);
        imp.login_holder.set_child(Some(&page));
    }

    /// Stops at the redirect, which carries the authorization code. Links that
    /// open new windows, such as Help, go to the browser.
    fn decide_login(
        &self,
        flow: &Rc<ha::auth::WebFlow>,
        decision: &webkit::PolicyDecision,
        kind: webkit::PolicyDecisionType,
    ) -> bool {
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
        if kind == webkit::PolicyDecisionType::NewWindowAction {
            decision.ignore();
            super::dashboard::launch(&uri);
            return true;
        }
        let Some(result) = flow.callback(&uri) else {
            return false;
        };
        decision.ignore();
        let flow = flow.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let Some(server) = obj.imp().server.borrow().clone() else {
                    return;
                };
                let auth = match result {
                    Ok(code) => flow.exchange(&code).await,
                    Err(err) => Err(err),
                };
                match auth {
                    Ok(auth) => obj.finish(server, auth),
                    Err(err) => {
                        obj.toast(&error_text(&err));
                        obj.start_login();
                    }
                }
            }
        ));
        true
    }

    #[template_callback]
    fn on_token_submit(&self) {
        let imp = self.imp();
        let token = imp.token_row.text().trim().to_owned();
        let Some(server) = imp.server.borrow().clone() else {
            return;
        };
        if token.is_empty() {
            return;
        }
        self.set_busy(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                match ha::auth::verify_token(&server, &token).await {
                    Ok(auth) => obj.finish(server, auth),
                    Err(err) => {
                        obj.set_busy(false);
                        obj.toast(&error_text(&err));
                    }
                }
            }
        ));
    }

    fn start_browser(&self) {
        let imp = self.imp();
        let Some(server) = imp.server.borrow().clone() else {
            return;
        };
        let flow = match ha::auth::LoopbackFlow::start(&server) {
            Ok(flow) => flow,
            Err(err) => {
                self.toast(&error_text(&err));
                return;
            }
        };
        imp.authorize_url.replace(flow.authorize_url().to_owned());
        let (cancel, cancelled) = oneshot::channel();
        imp.cancel_browser.replace(Some(cancel));
        imp.navigation.push_by_tag("browser");
        self.open_browser();

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let outcome = select(Box::pin(flow.finish()), cancelled).await;
                let Either::Left((result, _)) = outcome else {
                    return;
                };
                obj.imp().cancel_browser.take();
                match result {
                    Ok(auth) => obj.finish(server, auth),
                    Err(err) => {
                        obj.imp().navigation.pop_to_tag("login");
                        obj.toast(&error_text(&err));
                    }
                }
            }
        ));
    }

    #[template_callback]
    fn on_reopen_browser_clicked(&self) {
        self.open_browser();
    }

    fn open_browser(&self) {
        let url = self.imp().authorize_url.borrow().clone();
        log::debug!("Opening {url}");
        let window = self.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(&url).launch(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |result| {
                    if let Err(err) = result {
                        log::warn!("Could not open browser: {err}");
                        obj.toast(&gettext("Could not open a web browser"));
                    }
                }
            ),
        );
    }

    fn slot(&self, server: &ha::ServerUrl) -> DeviceSlot {
        self.imp()
            .device
            .borrow()
            .clone()
            .filter(|slot| slot.server == *server)
            .unwrap_or_else(|| DeviceSlot {
                server: server.clone(),
                device_id: ha::device::stable_id(),
                registration: None,
            })
    }

    /// A device that is already registered skips the name page.
    fn finish(&self, server: ha::ServerUrl, auth: ha::Auth) {
        let imp = self.imp();
        self.set_busy(false);
        self.stop_login();
        if self.slot(&server).registration.is_some() {
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                async move { obj.register(server, auth, true).await }
            ));
            return;
        }
        imp.pending.replace(Some((server, auth)));
        imp.navigation.push_by_tag("mode");
    }

    #[template_callback]
    fn on_mode_companion(&self) {
        let imp = self.imp();
        let name = imp.device_name.borrow().clone();
        imp.name_row.set_text(&if name.is_empty() {
            ha::device::suggested_name()
        } else {
            name
        });
        imp.navigation.push_by_tag("name");
        imp.name_row.grab_focus();
    }

    #[template_callback]
    fn on_mode_frontend(&self) {
        let Some((server, auth)) = self.imp().pending.take() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move { obj.register(server, auth, false).await }
        ));
    }

    #[template_callback]
    fn on_name_submit(&self) {
        let imp = self.imp();
        let name = imp.name_row.text().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let Some((server, auth)) = imp.pending.take() else {
            return;
        };
        imp.device_name.replace(name);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move { obj.register(server, auth, true).await }
        ));
    }

    /// Without `companion`, nothing is registered and Sanctum only shows
    /// Home Assistant.
    async fn register(&self, server: ha::ServerUrl, auth: ha::Auth, companion: bool) {
        let imp = self.imp();
        self.set_busy(true);
        imp.navigation.push_by_tag("finishing");

        let mut slot = self.slot(&server);
        let new_device = slot.registration.is_none();
        let registration = match slot.registration.clone() {
            Some(registration) => Some(registration),
            None if !companion => None,
            None => {
                let info = ha::device::DeviceInfo::detect(&imp.device_name.borrow());
                let result = ha::registration::register(
                    &server,
                    auth.access_token(),
                    &slot.device_id,
                    &info,
                )
                .await;
                match result {
                    Ok(registration) => {
                        slot.registration = Some(registration.clone());
                        imp.device.replace(Some(slot.clone()));
                        Some(registration)
                    }
                    Err(err) => {
                        imp.device.replace(Some(slot));
                        self.show_failure(&err);
                        return;
                    }
                }
            }
        };

        let token = auth.access_token().to_owned();
        let signed_in = server.clone();
        let credentials = Credentials {
            server,
            device_id: slot.device_id,
            auth,
            registration,
        };
        // Set first, so the window keeps this page while the frontend loads
        // behind it.
        imp.presenting.set(new_device);
        let device_name = if new_device {
            imp.device_name.borrow().clone()
        } else {
            String::new()
        };
        if let Err(err) = app().complete_sign_in(credentials, &device_name).await {
            imp.presenting.set(false);
            self.show_failure(&err);
            return;
        }
        imp.device.replace(None);
        imp.browser.replace(None);
        if new_device {
            self.show_connection(&signed_in, &token).await;
        }
        self.set_busy(false);
    }

    /// Asks the user to confirm the addresses found for the server and the
    /// current network. The page is left out when there is nothing to confirm.
    async fn show_connection(&self, server: &ha::ServerUrl, token: &str) {
        let imp = self.imp();
        let found = imp.found.borrow().clone();
        let addresses = glib::future_with_timeout(
            std::time::Duration::from_secs(5),
            discovery::addresses(server, token, &found),
        )
        .await
        .unwrap_or_default();
        let networks = ha::endpoints::current_networks().await.current().to_vec();

        let only_sign_in = [&addresses.internal, &addresses.external]
            .iter()
            .all(|url| url.is_empty() || url.as_str() == server.as_str());
        if networks.is_empty() && only_sign_in {
            self.show_background();
            return;
        }
        imp.internal_row.set_text(&addresses.internal);
        imp.external_row.set_text(&addresses.external);
        imp.network_group.set_visible(!networks.is_empty());
        imp.home_row.set_title(&glib::markup_escape_text(
            &gettext("Add “{}” to Home Networks").replace("{}", &networks.join(", ")),
        ));
        imp.home_row.set_active(addresses.at_home);
        imp.current_networks.replace(networks);
        imp.navigation.push_by_tag("connection");
        // An entry with focus would raise the on-screen keyboard.
        imp.connection_button.grab_focus();
    }

    #[template_callback]
    fn on_connection_skipped(&self) {
        self.show_background();
    }

    #[template_callback]
    fn on_connection_done(&self) {
        let imp = self.imp();
        let mut urls = Vec::new();
        for row in [&*imp.internal_row, &*imp.external_row] {
            let text = row.text().trim().to_owned();
            if text.is_empty() {
                urls.push(String::new());
                continue;
            }
            match ha::ServerUrl::parse(&text) {
                Ok(url) => urls.push(url.as_str().to_owned()),
                Err(err) => {
                    row.grab_focus();
                    self.toast(&error_text(&err));
                    return;
                }
            }
        }
        let settings = app().server();
        let _ = settings.set_string("internal-url", &urls[0]);
        let _ = settings.set_string("external-url", &urls[1]);
        if imp.home_row.is_active() {
            let mut networks: Vec<String> = settings
                .strv("home-networks")
                .iter()
                .map(ToString::to_string)
                .collect();
            for name in imp.current_networks.borrow().iter() {
                if !networks.contains(name) {
                    networks.push(name.clone());
                }
            }
            let _ = settings.set_strv("home-networks", networks);
        }
        self.show_background();
    }

    /// The background page is about notifications and sensors, which only
    /// a companion app has, as are two of the intro page's rows.
    fn show_background(&self) {
        let imp = self.imp();
        let companion = app().is_companion();
        imp.intro_sensors_row.set_visible(companion);
        imp.intro_controls_row.set_visible(companion);
        imp.intro_background_row.set_subtitle(&if companion {
            gettext("Keep notifications arriving after the window closes, and pick the default assistant")
        } else {
            gettext("Keep Sanctum ready after the window closes, and pick the default assistant")
        });
        imp.navigation
            .push_by_tag(if companion { "background" } else { "intro" });
    }

    #[template_callback]
    fn on_background_done(&self) {
        self.imp().navigation.push_by_tag("intro");
    }

    #[template_callback]
    fn on_intro_done(&self) {
        self.imp().presenting.set(false);
        if let Some(window) = self.root().and_downcast::<super::Window>() {
            window.update_page();
        }
    }

    fn show_failure(&self, error: &ha::Error) {
        self.imp().navigation.pop_to_tag("welcome");
        self.stop_login();
        self.set_busy(false);
        self.toast(&error_text(error));
    }
}
