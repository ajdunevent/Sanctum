use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use webkit::prelude::*;

use super::app;
use crate::application::Application;
use crate::ha::assist;
use crate::ha::endpoints;
use crate::ha::{ConnectionState, ServerUrl};

/// The choices of the Release After row; 0 never releases.
const UNLOAD_MINUTES: [u32; 5] = [10, 30, 60, 240, 0];

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/net/dunevent/Sanctum/ui/preferences-dialog.ui")]
    pub struct PreferencesDialog {
        #[template_child]
        pub background_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub rendering_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub motion_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub fps_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub keep_loaded_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub unload_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub titlebar_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub accent_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub pull_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub swipe_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub device_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub mode_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub background_group: TemplateChild<adw::PreferencesGroup>,
        /// Set while the mode row shows the current mode, so its handler
        /// does not treat that as a choice.
        pub mode_quiet: std::cell::Cell<bool>,
        /// The Sensors and Controls pages, shown only for a companion app.
        pub companion_pages: RefCell<Vec<adw::PreferencesPage>>,
        #[template_child]
        pub login_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub server_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub sign_out_row: TemplateChild<adw::ButtonRow>,
        #[template_child]
        pub default_pipeline_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub button_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub voice_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub tone_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub conversation_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub conversation_notice_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub launcher_row: TemplateChild<adw::SwitchRow>,
        /// Set while the launcher row shows the portal's state.
        pub launcher_quiet: std::cell::Cell<bool>,
        #[template_child]
        pub dismissal_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub dismissal_switch: TemplateChild<gtk::Switch>,
        #[template_child]
        pub internal_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub external_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub networks_row: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub add_network_row: TemplateChild<adw::ButtonRow>,
        #[template_child]
        pub add_network_entry: TemplateChild<adw::EntryRow>,
        pub network_rows: RefCell<Vec<adw::ActionRow>>,
        /// Setting values in row order: empty for "Last Used", then `assist::PREFERRED`,
        /// then pipeline IDs.
        pub pipeline_ids: RefCell<Vec<String>>,
        pub preferred_name: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "SanctumPreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_instance_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PreferencesDialog {}
    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
    impl PreferencesDialogImpl for PreferencesDialog {}
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends gtk::Widget, adw::Dialog, adw::PreferencesDialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

#[gtk::template_callbacks]
impl PreferencesDialog {
    pub fn new(app: &Application) -> Self {
        let dialog: Self = glib::Object::new();
        dialog.imp().companion_pages.replace(vec![
            super::sensors_page::build(app),
            super::controls_page::build(app),
        ]);
        let imp = dialog.imp();
        super::assistant_button::setup(&imp.button_group);
        let settings = app.settings();

        settings
            .bind("run-in-background", &*imp.background_row, "active")
            .build();
        settings
            .bind("start-at-login", &*imp.login_row, "active")
            .build();
        settings
            .bind("use-voice-assistant", &*imp.voice_row, "active")
            .build();
        settings
            .bind("assist-conversation-mode", &*imp.conversation_row, "active")
            .build();
        settings
            .bind(
                "assist-conversation-notice",
                &*imp.conversation_notice_row,
                "active",
            )
            .build();
        settings
            .bind("listening-tone", &*imp.tone_row, "active")
            .mapping(|value, _| Some((value.str()? != "none").to_value()))
            .set_mapping(|value, _| {
                Some(
                    if value.get::<bool>().ok()? {
                        "prompt"
                    } else {
                        "none"
                    }
                    .to_variant(),
                )
            })
            .build();
        imp.tone_row.connect_active_notify(|row| {
            if row.is_active() {
                crate::voice::listening_tone("prompt");
            }
        });
        settings
            .bind("web-rendering", &*imp.rendering_row, "selected")
            .mapping(|value, _| {
                let index: u32 = match value.get::<String>()?.as_str() {
                    "cpu" => 1,
                    "software" => 2,
                    _ => 0,
                };
                Some(index.to_value())
            })
            .set_mapping(|value, _| {
                let mode = match value.get::<u32>().ok()? {
                    1 => "cpu",
                    2 => "software",
                    _ => "gpu",
                };
                Some(mode.to_variant())
            })
            .build();
        settings
            .bind("web-reduce-motion", &*imp.motion_row, "active")
            .build();
        settings
            .bind("web-show-fps", &*imp.fps_row, "active")
            .build();
        settings
            .bind("keep-web-loaded", &*imp.keep_loaded_row, "active")
            .build();
        settings
            .bind("assist-dismissal", &*imp.dismissal_switch, "active")
            .build();
        dialog.setup_launcher_row();
        // A child row shows only while every row above it is on.
        let nested = glib::clone!(
            #[weak]
            dialog,
            move || {
                let imp = dialog.imp();
                let background = imp.background_row.is_active();
                imp.keep_loaded_row.set_visible(background);
                imp.login_row.set_visible(background);
                imp.unload_row
                    .set_visible(background && imp.keep_loaded_row.is_active());
                let conversation = imp.conversation_row.is_active();
                imp.conversation_notice_row.set_visible(conversation);
                imp.dismissal_row.set_visible(conversation);
            }
        );
        nested();
        for row in [
            &*imp.background_row,
            &*imp.keep_loaded_row,
            &*imp.conversation_row,
        ] {
            let nested = nested.clone();
            row.connect_active_notify(move |_| nested());
        }
        settings
            .bind("keep-web-loaded-minutes", &*imp.unload_row, "selected")
            .mapping(|value, _| {
                let minutes = value.get::<u32>()?;
                let index = UNLOAD_MINUTES
                    .iter()
                    .position(|m| *m == minutes)
                    .unwrap_or(1);
                Some((index as u32).to_value())
            })
            .set_mapping(|value, _| {
                let index = value.get::<u32>().ok()? as usize;
                UNLOAD_MINUTES
                    .get(index)
                    .map(|minutes| minutes.to_variant())
            })
            .build();
        settings
            .bind("integrated-titlebar", &*imp.titlebar_row, "active")
            .build();
        settings
            .bind("server-accent", &*imp.accent_row, "active")
            .build();
        settings
            .bind("pull-to-refresh", &*imp.pull_row, "active")
            .build();
        settings
            .bind("swipe-navigation", &*imp.swipe_row, "active")
            .build();

        let server_settings = app.server();
        let server = server_settings.string("url");
        if server.is_empty() {
            imp.server_row.set_subtitle(&gettext("Not signed in"));
            imp.sign_out_row.set_sensitive(false);
        } else {
            imp.server_row.set_subtitle(&server);
            imp.mode_row.set_visible(true);
        }
        dialog.show_mode();
        imp.mode_row.connect_selected_notify(glib::clone!(
            #[weak]
            dialog,
            move |row| {
                if dialog.imp().mode_quiet.get() {
                    return;
                }
                let companion = row.selected() == 0;
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    dialog,
                    async move { dialog.change_mode(companion).await }
                ));
            }
        ));
        imp.internal_row
            .set_text(&server_settings.string("internal-url"));
        imp.external_row
            .set_text(&server_settings.string("external-url"));
        dialog.show_networks();
        dialog.load_pipelines(app);
        dialog
    }

    fn load_pipelines(&self, app: &Application) {
        let row = &self.imp().default_pipeline_row;
        row.set_model(Some(&gtk::StringList::new(&[&gettext("Last Used")])));
        row.set_sensitive(false);

        let client = app.client().clone();
        if client.state() != ConnectionState::Connected {
            row.set_subtitle(&gettext("Connect to Home Assistant to choose"));
            return;
        }
        let current = app.server().string("assist-default-pipeline").to_string();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            async move {
                match assist::pipelines(&client).await {
                    Ok(pipelines) => {
                        let preferred = pipelines
                            .resolve(None)
                            .map(|pipeline| pipeline.name.clone());
                        dialog.imp().preferred_name.replace(preferred);
                        let mut entries = vec![
                            (String::new(), gettext("Last Used")),
                            (assist::PREFERRED.to_owned(), gettext("Preferred")),
                        ];
                        entries.extend(
                            pipelines
                                .pipelines
                                .into_iter()
                                .map(|pipeline| (pipeline.id, pipeline.name)),
                        );
                        if !entries.iter().any(|(id, _)| *id == current) {
                            entries.push((current.clone(), gettext("Unavailable Assistant")));
                        }
                        dialog.show_pipelines(entries, &current);
                    }
                    Err(err) => log::warn!("Could not list assistants: {err}"),
                }
            }
        ));
    }

    fn show_pipelines(&self, entries: Vec<(String, String)>, current: &str) {
        let imp = self.imp();
        let row = &imp.default_pipeline_row;
        let names: Vec<&str> = entries.iter().map(|(_, name)| name.as_str()).collect();
        row.set_model(Some(&gtk::StringList::new(&names)));
        let selected = entries
            .iter()
            .position(|(id, _)| id == current)
            .unwrap_or(0);
        row.set_selected(selected as u32);
        imp.pipeline_ids
            .replace(entries.into_iter().map(|(id, _)| id).collect());
        row.set_sensitive(true);
        self.update_pipeline_subtitle();

        row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| {
                let id = dialog
                    .imp()
                    .pipeline_ids
                    .borrow()
                    .get(row.selected() as usize)
                    .cloned()
                    .unwrap_or_default();
                let _ = app().server().set_string("assist-default-pipeline", &id);
                dialog.update_pipeline_subtitle();
            }
        ));
    }

    fn update_pipeline_subtitle(&self) {
        let imp = self.imp();
        let row = &imp.default_pipeline_row;
        let selected = imp
            .pipeline_ids
            .borrow()
            .get(row.selected() as usize)
            .cloned()
            .unwrap_or_default();
        let preferred = imp.preferred_name.borrow().clone();
        let subtitle = match (selected == assist::PREFERRED, preferred) {
            (true, Some(name)) => gettext("Follows the server's preferred assistant, currently {}")
                .replace("{}", &name),
            _ => gettext("Used when Assist opens"),
        };
        row.set_subtitle(&subtitle);
    }

    #[template_callback]
    fn on_rendering_info_activated(&self) {
        let web = webkit::WebView::builder().vexpand(true).build();
        super::dashboard::apply_settings(&web);
        web.load_uri("webkit://gpu");
        let view = adw::ToolbarView::builder().content(&web).build();
        view.add_top_bar(&adw::HeaderBar::new());
        let page = adw::NavigationPage::builder()
            .title(gettext("Rendering Information"))
            .child(&view)
            .build();
        self.push_subpage(&page);
    }

    /// Shows the current mode, with the device name and the Sensors and
    /// Controls pages only for a companion app.
    fn show_mode(&self) {
        let imp = self.imp();
        let app = app();
        let signed_in = !app.server().string("url").is_empty();
        let companion = app.is_companion();
        imp.mode_quiet.set(true);
        imp.mode_row.set_selected(if companion { 0 } else { 1 });
        imp.mode_quiet.set(false);
        imp.mode_row.set_subtitle(&if companion {
            gettext(
                "Sends sensor data to Home Assistant and receives its notifications and commands",
            )
        } else {
            gettext("Views and controls Home Assistant without the companion features")
        });
        imp.device_row.set_visible(signed_in && companion);
        imp.background_group.set_description(Some(&if companion {
            gettext("Running in the background keeps the connection open for notifications, sensors and the fastest Assist launch")
        } else {
            gettext("Running in the background keeps the connection open for the fastest Assist launch")
        }));
        if companion {
            imp.device_row.set_subtitle(&app.device_info().name);
        }
        for page in imp.companion_pages.borrow().iter() {
            let shown = page.parent().is_some();
            if companion && !shown {
                self.add(page);
            } else if !companion && shown {
                self.remove(page);
            }
        }
    }

    async fn change_mode(&self, companion: bool) {
        let result = if companion {
            match self.ask_device_name().await {
                Some(name) => app().use_as_companion(&name).await,
                None => {
                    self.show_mode();
                    return;
                }
            }
        } else {
            match self.confirm_frontend().await {
                Some(remove) => app().use_as_frontend(remove).await,
                None => {
                    self.show_mode();
                    return;
                }
            }
        };
        if let Err(err) = result {
            log::warn!("Could not change the app mode: {err}");
            let message = if companion {
                gettext("Could not register with Home Assistant")
            } else {
                gettext(
                    "Switched to front end, but the device could not be removed. Only an administrator can remove it, under Settings → Devices & Services.",
                )
            };
            self.add_toast(adw::Toast::builder().title(message).timeout(8).build());
        }
        self.show_mode();
    }

    async fn ask_device_name(&self) -> Option<String> {
        let previous = app().server().string("device-name").to_string();
        let entry = adw::EntryRow::builder()
            .title(gettext("Device Name"))
            .text(if previous.is_empty() {
                crate::ha::device::suggested_name()
            } else {
                previous
            })
            .activates_default(true)
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        list.append(&entry);
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Use as Companion App?"))
            .body(gettext(
                "Sanctum registers with Home Assistant under this name, which Home Assistant uses for its entities and its notify action.",
            ))
            .extra_child(&list)
            .default_response("register")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("register", &gettext("_Register")),
        ]);
        dialog.set_response_appearance("register", adw::ResponseAppearance::Suggested);
        if dialog.choose_future(Some(self)).await != "register" {
            return None;
        }
        Some(entry.text().trim().to_owned()).filter(|name| !name.is_empty())
    }

    /// `Some(true)` also removes the device from Home Assistant.
    async fn confirm_frontend(&self) -> Option<bool> {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Use as Front End?"))
            .body(gettext(
                "Sanctum will stop sending sensor data and location to Home Assistant and stop receiving its notifications and commands. Unless it is removed, this device stays listed in Home Assistant.",
            ))
            .default_response("keep")
            .close_response("cancel")
            .prefer_wide_layout(false)
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("keep", &gettext("_Keep Device Listed")),
            ("remove", &gettext("Re_move Device")),
        ]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        match dialog.choose_future(Some(self)).await.as_str() {
            "keep" => Some(false),
            "remove" => Some(true),
            _ => None,
        }
    }

    fn set_launcher_shown(&self, installed: bool) {
        let imp = self.imp();
        imp.launcher_quiet.set(true);
        imp.launcher_row.set_active(installed);
        imp.launcher_quiet.set(false);
    }

    fn setup_launcher_row(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            async move {
                if !crate::launcher::is_available().await {
                    return;
                }
                let installed = crate::launcher::is_installed().await;
                dialog.set_launcher_shown(installed);
                dialog.imp().launcher_row.set_visible(true);
            }
        ));
        self.imp().launcher_row.connect_active_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| {
                if dialog.imp().launcher_quiet.get() {
                    return;
                }
                let wanted = row.is_active();
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    dialog,
                    async move {
                        let result = if wanted {
                            crate::launcher::install(&gettext("Assist")).await
                        } else {
                            crate::launcher::uninstall().await.map(|()| true)
                        };
                        if let Err(err) = &result {
                            log::warn!("Could not change the Assist launcher: {err}");
                            dialog.add_toast(adw::Toast::new(&gettext(
                                "Could not change the Assist launcher",
                            )));
                        }
                        dialog.set_launcher_shown(crate::launcher::is_installed().await);
                    }
                ));
            }
        ));
    }

    #[template_callback]
    fn on_edit_dismissals(&self) {
        let settings = app().settings().clone();
        let list = adw::PreferencesGroup::builder()
            .title(gettext("Dismissal Phrases"))
            .description(gettext(
                "In Conversation Mode, Assist closes after replying when everything you say is made of these phrases, in any order or combination. Capitals and punctuation are ignored, and contractions match their long forms, so “that's all” also matches “that is all”.",
            ))
            .build();
        let add = adw::EntryRow::builder()
            .title(gettext("Add Phrase"))
            .show_apply_button(true)
            .build();
        let reset = adw::ButtonRow::builder()
            .title(gettext("_Reset to Defaults"))
            .use_underline(true)
            .build();
        let tools = adw::PreferencesGroup::new();
        tools.add(&add);
        tools.add(&reset);
        let page = adw::PreferencesPage::new();
        page.add(&list);
        page.add(&tools);
        let view = adw::ToolbarView::builder().content(&page).build();
        view.add_top_bar(&adw::HeaderBar::new());
        let subpage = adw::NavigationPage::builder()
            .title(gettext("Dismissal Phrases"))
            .child(&view)
            .build();

        let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::default();
        let show = {
            let settings = settings.clone();
            let list = list.clone();
            let rows = rows.clone();
            move || {
                for row in rows.take() {
                    list.remove(&row);
                }
                let phrases = settings.strv("assist-dismissal-phrases");
                for (index, phrase) in phrases.iter().enumerate() {
                    let row = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(phrase))
                        .build();
                    let remove = gtk::Button::builder()
                        .icon_name("user-trash-symbolic")
                        .tooltip_text(gettext("Remove"))
                        .valign(gtk::Align::Center)
                        .css_classes(["flat"])
                        .build();
                    remove.update_property(&[gtk::accessible::Property::Label(&gettext("Remove"))]);
                    let settings = settings.clone();
                    remove.connect_clicked(move |_| {
                        let mut phrases: Vec<String> = settings
                            .strv("assist-dismissal-phrases")
                            .iter()
                            .map(ToString::to_string)
                            .collect();
                        if index < phrases.len() {
                            phrases.remove(index);
                            let refs: Vec<&str> = phrases.iter().map(String::as_str).collect();
                            let _ = settings.set_strv("assist-dismissal-phrases", refs);
                        }
                    });
                    row.add_suffix(&remove);
                    list.add(&row);
                    rows.borrow_mut().push(row);
                }
            }
        };
        show();
        let handler =
            settings.connect_changed(Some("assist-dismissal-phrases"), move |_, _| show());
        let watched = settings.clone();
        let handler = RefCell::new(Some(handler));
        subpage.connect_hidden(move |_| {
            if let Some(handler) = handler.take() {
                watched.disconnect(handler);
            }
        });

        add.connect_apply({
            let settings = settings.clone();
            move |row| {
                let phrase = row.text().trim().to_owned();
                // Clearing counts as an edit, which would show the apply button again.
                row.set_show_apply_button(false);
                row.set_text("");
                row.set_show_apply_button(true);
                if phrase.is_empty() {
                    return;
                }
                let mut phrases: Vec<String> = settings
                    .strv("assist-dismissal-phrases")
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                if phrases
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&phrase))
                {
                    return;
                }
                phrases.push(phrase);
                let refs: Vec<&str> = phrases.iter().map(String::as_str).collect();
                let _ = settings.set_strv("assist-dismissal-phrases", refs);
            }
        });
        reset.connect_activated(move |_| settings.reset("assist-dismissal-phrases"));
        self.push_subpage(&subpage);
    }

    #[template_callback]
    fn on_internal_applied(&self) {
        self.apply_url(&self.imp().internal_row, "internal-url");
    }

    #[template_callback]
    fn on_external_applied(&self) {
        self.apply_url(&self.imp().external_row, "external-url");
    }

    fn apply_url(&self, row: &adw::EntryRow, key: &str) {
        let text = row.text().trim().to_owned();
        let value = if text.is_empty() {
            String::new()
        } else {
            match ServerUrl::parse(&text) {
                Ok(url) => url.to_string(),
                Err(_) => {
                    self.add_toast(adw::Toast::new(&gettext("Enter a valid server address")));
                    return;
                }
            }
        };
        let _ = app().server().set_string(key, &value);
    }

    fn networks(&self) -> Vec<String> {
        app()
            .server()
            .strv("home-networks")
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn save_networks(&self, networks: &[String]) {
        let refs: Vec<&str> = networks.iter().map(String::as_str).collect();
        let _ = app().server().set_strv("home-networks", refs);
        self.show_networks();
    }

    fn show_networks(&self) {
        let imp = self.imp();
        for row in imp.network_rows.take() {
            imp.networks_row.remove(&row);
        }
        let networks = self.networks();
        imp.networks_row.set_subtitle(&if networks.is_empty() {
            gettext("None")
        } else {
            networks.join(", ")
        });
        let mut rows = Vec::new();
        for ssid in networks {
            let row = adw::ActionRow::builder().title(&ssid).build();
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(gettext("Remove"))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            super::label_by_tooltip(&remove);
            remove.connect_clicked(glib::clone!(
                #[weak(rename_to = dialog)]
                self,
                move |_| {
                    let remaining: Vec<String> = dialog
                        .networks()
                        .into_iter()
                        .filter(|n| *n != ssid)
                        .collect();
                    dialog.save_networks(&remaining);
                }
            ));
            row.add_suffix(&remove);
            imp.networks_row.add_row(&row);
            rows.push(row);
        }
        // Re-added so it stays below the networks.
        imp.networks_row.remove(&*imp.add_network_row);
        imp.networks_row.add_row(&*imp.add_network_row);
        imp.networks_row.remove(&*imp.add_network_entry);
        imp.networks_row.add_row(&*imp.add_network_entry);
        imp.network_rows.replace(rows);
    }

    #[template_callback]
    fn on_add_network_activated(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            async move {
                let networks = endpoints::current_networks().await;
                if networks.current().is_empty() {
                    dialog.add_toast(adw::Toast::new(&gettext("Not connected to a network")));
                    return;
                }
                dialog.add_networks(networks.current());
            }
        ));
    }

    #[template_callback]
    fn on_network_entered(&self) {
        let entry = &self.imp().add_network_entry;
        let name = entry.text().trim().to_owned();
        if !name.is_empty() {
            self.add_networks(&[name]);
        }
        entry.set_text("");
    }

    fn add_networks(&self, names: &[String]) {
        let mut networks = self.networks();
        for name in names {
            if !networks.contains(name) {
                networks.push(name.clone());
            }
        }
        self.save_networks(&networks);
    }

    #[template_callback]
    fn on_sign_out_activated(&self) {
        let alert = adw::AlertDialog::builder()
            .heading(gettext("Sign Out?"))
            .body(gettext(
                "Sanctum will forget this server and its credentials. The device stays listed in Home Assistant until you remove it there.",
            ))
            .default_response("cancel")
            .close_response("cancel")
            .build();
        alert.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("sign-out", &gettext("_Sign Out")),
        ]);
        alert.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
        alert.connect_response(
            Some("sign-out"),
            glib::clone!(
                #[weak(rename_to = dialog)]
                self,
                move |_, _| {
                    glib::spawn_future_local(async move {
                        app().sign_out().await;
                        dialog.close();
                    });
                }
            ),
        );
        alert.present(Some(self));
    }
}
