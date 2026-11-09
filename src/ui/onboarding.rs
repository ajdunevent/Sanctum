use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use futures_channel::oneshot;
use futures_util::future::{Either, select};
use gettextrs::gettext;
use gtk::glib;
use serde_json::{Map, Value};

use super::{app, confirm_trust, error_text};
use crate::ha::credentials::Registration;
use crate::ha::discovery;
use crate::ha::login::{Field, Provider, Step};
use crate::ha::{self, Credentials};

/// A login flow in progress.
#[derive(Clone, Debug)]
pub struct LoginState {
    provider: Provider,
    flow_id: String,
}

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
        pub login_title: TemplateChild<gtk::Label>,
        #[template_child]
        pub login_subtitle: TemplateChild<gtk::Label>,
        #[template_child]
        pub login_spinner: TemplateChild<adw::Spinner>,
        #[template_child]
        pub login_form: TemplateChild<gtk::Box>,
        #[template_child]
        pub login_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub others_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub others_heading: TemplateChild<gtk::Label>,
        #[template_child]
        pub others_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub token_row: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub token_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub name_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub search_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub search_spinner: TemplateChild<adw::Spinner>,
        #[template_child]
        pub search_label: TemplateChild<gtk::Label>,
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
        /// The server's sign-in providers, as `/auth/providers` lists them.
        pub providers: RefCell<Vec<Provider>>,
        pub flow: RefCell<Option<LoginState>>,
        /// The current form's text fields, by field name.
        pub login_entries: RefCell<Vec<(String, adw::EntryRow)>>,
        /// The button starts a new flow after a flow fails or stops.
        pub login_restart: Cell<bool>,
        /// Lets a request tell whether the page moved on while it waited.
        pub login_generation: Cell<u32>,
        pub authorize_url: RefCell<String>,
        pub cancel_browser: RefCell<Option<oneshot::Sender<()>>>,
        /// The welcome page stays up after sign-in until it is dismissed.
        pub presenting: Cell<bool>,
        pub browser: RefCell<Option<discovery::Browser>>,
        /// Servers announced on the local network, in list order.
        pub found: RefCell<Vec<discovery::Found>>,
        /// Lets a search's timeout tell whether a newer search replaced it.
        pub search: Cell<u32>,
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
        let search = imp.search.get().wrapping_add(1);
        imp.search.set(search);
        self.show_search_status(&gettext("Searching this network for Home Assistant…"), true);
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
                if browser.is_none() && obj.imp().search.get() == search {
                    obj.show_search_status(
                        &gettext(
                            "Automatic detection is not available on this device. Please enter the address above.",
                        ),
                        false,
                    );
                }
                obj.imp().browser.replace(browser);
            }
        ));
        glib::timeout_add_seconds_local_once(
            10,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move || {
                    let imp = obj.imp();
                    if imp.search.get() == search
                        && imp.found.borrow().is_empty()
                        && imp.search_spinner.is_visible()
                    {
                        obj.show_search_status(
                            &gettext(
                                "Automatic detection did not identify a Home Assistant server on this network. Please enter the address above.",
                            ),
                            false,
                        );
                    }
                }
            ),
        );
    }

    fn show_search_status(&self, text: &str, searching: bool) {
        let imp = self.imp();
        imp.search_label.set_label(text);
        imp.search_spinner.set_visible(searching);
        imp.search_box.set_visible(true);
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
        imp.search_box.set_visible(false);
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

    /// Shows the sign-in page, led by the first provider Sanctum can show
    /// natively.
    fn start_login(&self) {
        let imp = self.imp();
        let Some(server) = imp.server.borrow().clone() else {
            return;
        };
        self.stop_login();
        let generation = imp.login_generation.get();
        self.show_login_loading();
        if imp.navigation.visible_page_tag().as_deref() != Some("login") {
            imp.navigation.push_by_tag("login");
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let providers = ha::login::providers(&server).await;
                let imp = obj.imp();
                if imp.login_generation.get() != generation {
                    return;
                }
                let providers = providers.unwrap_or_else(|err| {
                    log::debug!("No sign-in providers: {err}");
                    Vec::new()
                });
                let primary = ha::login::primary(&providers).cloned();
                imp.providers.replace(providers);
                match primary {
                    Some(provider) => obj.begin_flow(provider).await,
                    None => obj.show_ways(),
                }
            }
        ));
    }

    /// Ends the flow in progress and drops any answer still on its way.
    fn stop_login(&self) {
        let imp = self.imp();
        imp.login_generation
            .set(imp.login_generation.get().wrapping_add(1));
        let server = imp.server.borrow().clone();
        if let (Some(state), Some(server)) = (imp.flow.take(), server) {
            glib::spawn_future_local(async move {
                ha::login::cancel(&server, &state.flow_id).await;
            });
        }
    }

    async fn begin_flow(&self, provider: Provider) {
        let imp = self.imp();
        let Some(server) = imp.server.borrow().clone() else {
            return;
        };
        self.stop_login();
        let generation = imp.login_generation.get();
        self.show_login_loading();
        let step = ha::login::start(&server, &provider).await;
        if imp.login_generation.get() == generation {
            self.show_step(provider, step);
        }
    }

    fn show_step(&self, provider: Provider, step: ha::Result<Step>) {
        let imp = self.imp();
        self.set_login_busy(false);
        match step {
            Ok(Step::Form {
                flow_id,
                step_id,
                fields,
                error,
                placeholders,
            }) => {
                if !renderable(&fields) {
                    log::debug!("Sign-in step {step_id} has fields Sanctum cannot show");
                    self.hand_to_browser(&flow_id);
                    return;
                }
                imp.flow.replace(Some(LoginState {
                    provider: provider.clone(),
                    flow_id,
                }));
                self.render_form(
                    &provider,
                    &step_id,
                    &fields,
                    error.as_deref(),
                    &placeholders,
                );
            }
            Ok(Step::Done(code)) => {
                imp.flow.replace(None);
                self.complete_login(code);
            }
            Ok(Step::Abort(reason)) => {
                imp.flow.replace(None);
                self.show_login_failure(&provider, &abort_text(&reason));
            }
            Ok(Step::Unsupported(kind)) => {
                log::debug!("Sign-in step of type {kind} needs the browser");
                let flow_id = imp
                    .flow
                    .borrow()
                    .as_ref()
                    .map(|state| state.flow_id.clone());
                self.hand_to_browser(flow_id.as_deref().unwrap_or_default());
            }
            Err(err) => {
                imp.flow.replace(None);
                self.show_login_failure(&provider, &error_text(&err));
            }
        }
    }

    /// Cancels the flow and continues in the browser, which can show any step.
    fn hand_to_browser(&self, flow_id: &str) {
        let imp = self.imp();
        imp.flow.replace(None);
        if let Some(server) = imp.server.borrow().clone().filter(|_| !flow_id.is_empty()) {
            let flow_id = flow_id.to_owned();
            glib::spawn_future_local(async move {
                ha::login::cancel(&server, &flow_id).await;
            });
        }
        self.toast(&gettext("This sign-in continues in your web browser"));
        self.start_browser();
    }

    fn server_title(&self) -> String {
        let imp = self.imp();
        let server = imp.server.borrow().clone();
        let name = server.as_ref().and_then(|server| {
            imp.found
                .borrow()
                .iter()
                .find(|found| {
                    ha::ServerUrl::parse(&found.internal_url).ok().as_ref() == Some(server)
                })
                .map(|found| found.name.clone())
        });
        match name {
            Some(name) => gettext("Sign In to {}").replace("{}", &name),
            None => gettext("Sign In to Home Assistant"),
        }
    }

    fn set_login_text(&self, title: &str, subtitle: &str) {
        let imp = self.imp();
        imp.login_title.set_label(title);
        imp.login_subtitle.set_label(subtitle);
        imp.login_subtitle.set_visible(!subtitle.is_empty());
    }

    fn clear_login_form(&self) {
        let imp = self.imp();
        while let Some(child) = imp.login_form.first_child() {
            imp.login_form.remove(&child);
        }
        imp.login_entries.borrow_mut().clear();
        imp.login_button.set_visible(false);
        imp.login_restart.set(false);
    }

    fn show_login_loading(&self) {
        let imp = self.imp();
        self.clear_login_form();
        let address = imp
            .server
            .borrow()
            .as_ref()
            .map(|server| server.as_str().to_owned())
            .unwrap_or_default();
        self.set_login_text(&self.server_title(), &address);
        imp.login_spinner.set_visible(true);
        imp.others_box.set_visible(false);
    }

    fn set_login_busy(&self, busy: bool) {
        let imp = self.imp();
        imp.login_spinner.set_visible(false);
        imp.login_form.set_sensitive(!busy);
        imp.login_button.set_sensitive(!busy);
        imp.others_list.set_sensitive(!busy);
    }

    fn render_form(
        &self,
        provider: &Provider,
        step_id: &str,
        fields: &[Field],
        error: Option<&str>,
        placeholders: &std::collections::HashMap<String, String>,
    ) {
        let imp = self.imp();
        // A step shown again after an error keeps what was typed.
        let typed: std::collections::HashMap<String, String> = imp
            .login_entries
            .borrow()
            .iter()
            .map(|(name, row)| (name.clone(), row.text().to_string()))
            .collect();
        self.clear_login_form();
        let (title, subtitle) = match step_id {
            "mfa" => (
                gettext("Two-Factor Authentication"),
                if placeholders.get("mfa_module_id").map(String::as_str) == Some("totp") {
                    gettext("Enter the code from your authenticator app")
                } else {
                    gettext("Enter the code from {}").replace(
                        "{}",
                        placeholders
                            .get("mfa_module_name")
                            .map(String::as_str)
                            .unwrap_or_default(),
                    )
                },
            ),
            "select_mfa_module" => (
                gettext("Two-Factor Authentication"),
                gettext("Choose how to confirm it’s you"),
            ),
            _ if provider.kind == "trusted_networks" => (
                self.server_title(),
                gettext(
                    "Choose who to sign in as. This network is trusted, so no password is needed.",
                ),
            ),
            _ => (
                self.server_title(),
                imp.server
                    .borrow()
                    .as_ref()
                    .map(|server| server.as_str().to_owned())
                    .unwrap_or_default(),
            ),
        };
        self.set_login_text(&title, &subtitle);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        if let [Field::Select { name, options }] = fields {
            for (value, label) in options {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(label))
                    .activatable(true)
                    .build();
                if name == "user" {
                    row.add_prefix(
                        &adw::Avatar::builder()
                            .size(32)
                            .text(label)
                            .show_initials(true)
                            .build(),
                    );
                }
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let mut values = Map::new();
                values.insert(name.clone(), Value::String(value.clone()));
                row.connect_activated(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_| obj.submit_login(values.clone())
                ));
                list.append(&row);
            }
        } else {
            let mut entries = Vec::new();
            for field in fields {
                let name = field.name().to_owned();
                let row: adw::EntryRow = if name == "password" {
                    adw::PasswordEntryRow::new().upcast()
                } else {
                    adw::EntryRow::new()
                };
                row.set_title(&field_label(&name));
                if let Some(text) = typed.get(&name) {
                    row.set_text(text);
                }
                if name == "code" {
                    row.set_input_purpose(gtk::InputPurpose::Digits);
                }
                if name == "username" {
                    row.set_input_hints(
                        gtk::InputHints::NO_SPELLCHECK | gtk::InputHints::LOWERCASE,
                    );
                }
                row.connect_entry_activated(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_| obj.on_login_clicked()
                ));
                list.append(&row);
                entries.push((name, row));
            }
            imp.login_button.set_label(&if step_id == "init" {
                gettext("_Sign In")
            } else {
                gettext("_Verify")
            });
            imp.login_button.set_visible(true);
            imp.login_entries.replace(entries);
        }
        imp.login_form.append(&list);
        // After an error the last field, usually the password or code, is
        // selected for retyping.
        let entries = imp.login_entries.borrow();
        let target = if error.is_some() {
            entries.last()
        } else {
            entries.iter().find(|(_, row)| row.text().is_empty())
        };
        if let Some((_, row)) = target {
            row.grab_focus();
            row.select_region(0, -1);
        }
        drop(entries);
        if let Some(error) = error {
            imp.login_form.append(&error_label(&form_error_text(error)));
        }
        self.fill_others(Some(provider));
    }

    fn show_login_failure(&self, provider: &Provider, message: &str) {
        let imp = self.imp();
        self.clear_login_form();
        imp.login_form.append(&error_label(message));
        imp.login_button.set_label(&gettext("_Start Again"));
        imp.login_button.set_visible(true);
        imp.login_restart.set(true);
        self.fill_others(Some(provider));
    }

    /// The page for servers whose providers all need the browser.
    fn show_ways(&self) {
        let imp = self.imp();
        self.set_login_busy(false);
        self.clear_login_form();
        self.set_login_text(
            &self.server_title(),
            &gettext("This server’s sign-in methods need your browser or an access token."),
        );
        imp.flow.replace(None);
        self.fill_others(None);
    }

    /// Lists every way in except `current`: collapsed under "Other Ways to
    /// Sign In" when a method is shown, or open as "Ways to Sign In".
    fn fill_others(&self, current: Option<&Provider>) {
        let imp = self.imp();
        imp.others_list.remove_all();
        let mut rows = Vec::new();
        for provider in imp.providers.borrow().iter() {
            if Some(provider) == current {
                continue;
            }
            rows.push(self.provider_row(provider));
        }
        let browser = way_row(
            &gettext("Use Your Browser"),
            &gettext("Sign in on Home Assistant’s own page"),
            "adw-external-link-symbolic",
        );
        browser.connect_activated(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| obj.start_browser()
        ));
        rows.push(browser);
        let token = way_row(
            &gettext("Use an Access Token"),
            &gettext("Paste a long-lived token from your profile"),
            "go-next-symbolic",
        );
        token.connect_activated(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| {
                let imp = obj.imp();
                imp.navigation.push_by_tag("token");
                imp.token_row.grab_focus();
            }
        ));
        rows.push(token);

        if current.is_some() {
            let expander = adw::ExpanderRow::builder()
                .title(gettext("Other Ways to Sign In"))
                .build();
            for row in &rows {
                expander.add_row(row);
            }
            imp.others_list.append(&expander);
        } else {
            for row in &rows {
                imp.others_list.append(row);
            }
        }
        imp.others_heading.set_visible(current.is_none());
        imp.others_box.set_visible(true);
    }

    fn provider_row(&self, provider: &Provider) -> adw::ActionRow {
        if !provider.is_native() {
            let row = way_row(
                &provider.name,
                &gettext("Opens in your browser"),
                "adw-external-link-symbolic",
            );
            row.connect_activated(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |_| obj.start_browser()
            ));
            return row;
        }
        let subtitle = match provider.kind.as_str() {
            "trusted_networks" => gettext("Choose a user without a password"),
            _ => gettext("Username and password"),
        };
        let row = way_row(&provider.name, &subtitle, "go-next-symbolic");
        let provider = provider.clone();
        row.connect_activated(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| {
                let provider = provider.clone();
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    obj,
                    async move { obj.begin_flow(provider).await }
                ));
            }
        ));
        row
    }

    #[template_callback]
    fn on_login_clicked(&self) {
        let imp = self.imp();
        if imp.login_restart.get() {
            let provider = imp
                .flow
                .borrow()
                .as_ref()
                .map(|state| state.provider.clone());
            match provider.or_else(|| ha::login::primary(&imp.providers.borrow()).cloned()) {
                Some(provider) => glib::spawn_future_local(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    async move { obj.begin_flow(provider).await }
                )),
                None => {
                    self.start_login();
                    return;
                }
            };
            return;
        }
        let mut values = Map::new();
        for (name, row) in imp.login_entries.borrow().iter() {
            let text = row.text().to_string();
            if text.is_empty() {
                row.grab_focus();
                return;
            }
            values.insert(name.clone(), Value::String(text));
        }
        self.submit_login(values);
    }

    fn submit_login(&self, values: Map<String, Value>) {
        let imp = self.imp();
        let (Some(state), Some(server)) = (imp.flow.borrow().clone(), imp.server.borrow().clone())
        else {
            return;
        };
        let generation = imp.login_generation.get();
        self.set_login_busy(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                let step = ha::login::submit(&server, &state.flow_id, values).await;
                if obj.imp().login_generation.get() == generation {
                    obj.show_step(state.provider, step);
                }
            }
        ));
    }

    fn complete_login(&self, code: String) {
        let imp = self.imp();
        let Some(server) = imp.server.borrow().clone() else {
            return;
        };
        self.set_login_busy(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            async move {
                match ha::login::finish(&server, &code).await {
                    Ok(auth) => obj.finish(server, auth),
                    Err(err) => {
                        obj.set_login_busy(false);
                        let provider = ha::login::primary(&obj.imp().providers.borrow()).cloned();
                        match provider {
                            Some(provider) => obj.show_login_failure(&provider, &error_text(&err)),
                            None => obj.toast(&error_text(&err)),
                        }
                    }
                }
            }
        ));
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

/// Forms are either text fields or a single choice list.
fn renderable(fields: &[Field]) -> bool {
    matches!(fields, [Field::Select { .. }])
        || (!fields.is_empty()
            && fields
                .iter()
                .all(|field| matches!(field, Field::Text { .. })))
}

fn field_label(name: &str) -> String {
    match name {
        "username" => gettext("Username"),
        "password" => gettext("Password"),
        "code" => gettext("Code"),
        other => other.to_owned(),
    }
}

fn form_error_text(key: &str) -> String {
    match key {
        "invalid_auth" => gettext("Invalid username or password"),
        "invalid_code" => gettext("Invalid code"),
        "invalid_auth_module" => gettext("Choose a two-factor method"),
        other => format!("{} ({other})", gettext("Sign-in failed")),
    }
}

fn abort_text(reason: &str) -> String {
    match reason {
        "not_allowed" => gettext("This network may not sign in this way"),
        "login_expired" => gettext("The sign-in took too long"),
        "too_many_retry" => gettext("Too many wrong codes"),
        other => format!("{} ({other})", gettext("Sign-in stopped")),
    }
}

fn error_label(message: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(message)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["error", "caption"])
        .build()
}

fn way_row(title: &str, subtitle: &str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(title))
        .subtitle(glib::markup_escape_text(subtitle))
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name(icon));
    row
}
