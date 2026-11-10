use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use futures_util::StreamExt;
use gettextrs::gettext;
use gtk::{gio, glib};

use super::error_text;
use crate::application::Application;
use crate::ha::assist::{self, Pipelines, VoiceEvent};
use crate::ha::{self, ConnectionState};
use crate::voice;

/// The server ends listening when it hears the end of speech; this caps a
/// turn where it never does, such as in a noisy room.
const MAX_LISTEN_SECONDS: u32 = 15;

const CONVERSATION_KEY: &str = "assist-conversation-mode";

/// One spoken question, from opening the microphone to the reply.
#[derive(Debug, Default)]
struct Turn {
    recorder: Option<voice::Recorder>,
    /// Prefixes every audio frame; known once the run starts.
    handler: Option<u8>,
    /// Audio captured before the run started.
    buffered: Vec<u8>,
    audio_ended: bool,
    /// Captured audio still to drop so the listening tone is not transcribed.
    skip: usize,
    subscription: Option<u64>,
    timeout: Option<glib::SourceId>,
}

#[derive(Clone, Copy)]
enum Speaker {
    User,
    Assist,
    Error,
    Note,
}

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/net/dunevent/Sanctum/ui/assist-window.ui")]
    pub struct AssistWindow {
        #[template_child]
        pub pipeline_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub transcript: TemplateChild<gtk::Box>,
        #[template_child]
        pub entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub voice_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub speaker_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub conversation_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub voice_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub voice_label: TemplateChild<gtk::Label>,

        pub conversation_id: RefCell<Option<String>>,
        /// `None` lets the server use its preferred pipeline.
        pub pipeline_id: RefCell<Option<String>>,
        pub pipelines: RefCell<Pipelines>,
        pub pipeline_action: RefCell<Option<gio::SimpleAction>>,
        /// Typed before the connection and pipeline list are ready; sent in order afterwards.
        pub queued: RefCell<Vec<String>>,
        /// Set once the pipeline list for this connection has loaded.
        pub ready: Cell<bool>,
        pub client_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub(super) turn: RefCell<Option<Turn>>,
        /// Bumped per turn; tasks from a cancelled turn stop on a mismatch.
        pub turn_generation: Cell<u64>,
        /// Set while the toggle shows state, so its handler ignores the change.
        pub voice_quiet: Cell<bool>,
        /// Set while the conversation toggle shows the setting.
        pub conversation_quiet: Cell<bool>,
        pub listen_when_ready: Cell<bool>,
        /// Set by a reply in conversation mode; listening resumes once the
        /// reply is done.
        pub follow_up: Cell<bool>,
        /// A spoken reply is downloading or playing.
        pub reply_audio: Cell<bool>,
        /// The user said goodbye in conversation mode; the window closes once
        /// the reply is done.
        pub closing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AssistWindow {
        const NAME: &'static str = "SanctumAssistWindow";
        type Type = super::AssistWindow;
        type ParentType = adw::Window;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_instance_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for AssistWindow {
        fn constructed(&self) {
            self.parent_constructed();
        }

        fn dispose(&self) {
            self.obj().cancel_turn();
            if let Some(app) = self.obj().app() {
                for handler in self.client_handlers.take() {
                    app.client().disconnect(handler);
                }
            }
            self.dispose_template();
        }
    }

    impl WidgetImpl for AssistWindow {}
    impl WindowImpl for AssistWindow {}
    impl AdwWindowImpl for AssistWindow {}
}

glib::wrapper! {
    pub struct AssistWindow(ObjectSubclass<imp::AssistWindow>)
        @extends gtk::Widget, gtk::Window, adw::Window,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

#[gtk::template_callbacks]
impl AssistWindow {
    pub fn new(app: &Application) -> Self {
        let window: Self = glib::Object::builder().property("application", app).build();
        let imp = window.imp();

        let settings = app.server();
        let wanted = assist::wanted_pipeline(
            &settings.string("assist-default-pipeline"),
            &settings.string("assist-last-pipeline"),
        );
        imp.pipeline_id.replace(wanted.clone());
        window.setup_pipeline_action(wanted.unwrap_or_default());

        let client = app.client();
        let handlers = vec![
            client.connect_state_notify(glib::clone!(
                #[weak]
                window,
                move |_| window.on_state_changed()
            )),
            client.connect_location_name_notify(glib::clone!(
                #[weak]
                window,
                move |_| window.update_subtitle()
            )),
        ];
        imp.client_handlers.replace(handlers);
        app.settings()
            .bind("assist-speak-replies", &*imp.speaker_button, "active")
            .build();
        app.settings().connect_changed(
            Some(CONVERSATION_KEY),
            glib::clone!(
                #[weak]
                window,
                move |_, _| window.show_conversation_mode()
            ),
        );
        window.update_speaker_icon();
        window.show_conversation_mode();
        window.greet();
        window.connect_close_request(|window| {
            window.cancel_turn();
            glib::Propagation::Proceed
        });
        GtkWindowExt::set_focus(&window, Some(&*imp.entry));
        window.on_state_changed();
        window
    }

    fn app(&self) -> Option<Application> {
        self.application().and_downcast()
    }

    fn setup_pipeline_action(&self, initial: String) {
        let action = gio::SimpleAction::new_stateful(
            "pipeline",
            Some(glib::VariantTy::STRING),
            &initial.to_variant(),
        );
        action.connect_change_state(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |action, value| {
                let Some(id) = value.and_then(|v| v.get::<String>()) else {
                    return;
                };
                action.set_state(&id.to_variant());
                window.select_pipeline(id);
            }
        ));
        let group = gio::SimpleActionGroup::new();
        group.add_action(&action);
        self.insert_action_group("assist", Some(&group));
        self.imp().pipeline_action.replace(Some(action));
    }

    fn update_subtitle(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let client = app.client();
        let subtitle = match client.state() {
            ConnectionState::Connected => client.location_name().to_string(),
            ConnectionState::Connecting => gettext("Connecting…"),
            ConnectionState::Disconnected => gettext("Not connected"),
            ConnectionState::AuthFailed => gettext("Sign in again in Sanctum"),
            ConnectionState::Idle if app.is_signed_in() => gettext("Connecting…"),
            ConnectionState::Idle => gettext("Not signed in"),
        };
        self.imp().window_title.set_subtitle(&subtitle);
    }

    fn on_state_changed(&self) {
        self.update_subtitle();
        let Some(app) = self.app() else {
            return;
        };
        let imp = self.imp();
        if app.client().state() == ConnectionState::Connected {
            if !imp.ready.get() {
                self.load_pipelines();
            }
        } else {
            imp.ready.set(false);
            imp.pipeline_button.set_sensitive(false);
            if imp.turn.borrow().is_some() {
                self.cancel_turn();
            }
        }
    }

    /// Validates the chosen pipeline against the server, then releases queued input.
    fn load_pipelines(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let client = app.client().clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                match assist::pipelines(&client).await {
                    Ok(pipelines) => window.set_pipelines(pipelines),
                    Err(err) => log::warn!("Could not list assistants: {err}"),
                }
                if client.state() != ConnectionState::Connected {
                    return;
                }
                let imp = window.imp();
                imp.ready.set(true);
                for text in imp.queued.take() {
                    window.ask(text);
                }
                if imp.listen_when_ready.take() {
                    window.start_turn(true);
                }
            }
        ));
    }

    fn set_pipelines(&self, pipelines: Pipelines) {
        let imp = self.imp();
        let current = imp.pipeline_id.borrow().clone();
        let resolved = pipelines
            .resolve(current.as_deref())
            .map(|pipeline| pipeline.id.clone());

        let menu = gio::Menu::new();
        for pipeline in &pipelines.pipelines {
            menu.append(
                Some(&pipeline.name),
                Some(&format!("assist.pipeline::{}", pipeline.id)),
            );
        }
        imp.pipeline_button.set_menu_model(Some(&menu));
        imp.pipeline_button
            .set_sensitive(pipelines.pipelines.len() > 1);
        imp.pipelines.replace(pipelines);

        imp.pipeline_id.replace(resolved.clone());
        if let Some(action) = imp.pipeline_action.borrow().as_ref() {
            action.set_state(&resolved.unwrap_or_default().to_variant());
        }
        self.update_title();
    }

    fn update_title(&self) {
        let imp = self.imp();
        let name = imp
            .pipeline_id
            .borrow()
            .as_deref()
            .and_then(|id| imp.pipelines.borrow().get(id).map(|p| p.name.clone()));
        imp.window_title
            .set_title(&name.unwrap_or_else(|| gettext("Assist")));
    }

    fn select_pipeline(&self, id: String) {
        let imp = self.imp();
        if imp.pipeline_id.borrow().as_deref() == Some(id.as_str()) {
            return;
        }
        imp.pipeline_id.replace(Some(id.clone()));
        self.remember_pipeline(&id);
        self.update_title();
        // Conversation IDs belong to one agent.
        self.clear_conversation();
    }

    fn remember_pipeline(&self, id: &str) {
        if let Some(app) = self.app() {
            let _ = app.server().set_string("assist-last-pipeline", id);
        }
    }

    #[template_callback]
    fn on_send(&self) {
        let imp = self.imp();
        let text = imp.entry.text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        // Typing takes over from a spoken question in progress.
        self.cancel_turn();
        imp.entry.set_text("");
        self.add_bubble(&text, Speaker::User);

        let Some(app) = self.app() else {
            return;
        };
        if imp.ready.get() {
            self.ask(text);
        } else if app.is_signed_in() {
            imp.queued.borrow_mut().push(text);
        } else {
            self.add_bubble(&gettext("Sign in to Sanctum to use Assist"), Speaker::Error);
        }
    }

    fn clear_conversation(&self) {
        let imp = self.imp();
        self.cancel_turn();
        imp.conversation_id.replace(None);
        imp.queued.borrow_mut().clear();
        while let Some(child) = imp.transcript.first_child() {
            imp.transcript.remove(&child);
        }
        self.greet();
        imp.entry.grab_focus();
    }

    fn greet(&self) {
        self.add_bubble(&gettext("How can I assist?"), Speaker::Assist);
    }

    #[template_callback]
    fn on_speaker_toggled(&self) {
        self.update_speaker_icon();
        if !self.imp().speaker_button.is_active() {
            voice::stop_playback();
        }
    }

    fn set_conversation_mode(&self, on: bool) {
        if let Some(app) = self.app() {
            let _ = app.settings().set_boolean(CONVERSATION_KEY, on);
        }
    }

    fn show_conversation_mode(&self) {
        let imp = self.imp();
        let on = self.conversation_mode();
        imp.conversation_quiet.set(true);
        imp.conversation_button.set_active(on);
        imp.conversation_quiet.set(false);
        imp.conversation_button.set_icon_name(if on {
            "speaking-head-symbolic"
        } else {
            "speaking-head-disabled-symbolic"
        });
        if !on {
            imp.follow_up.set(false);
        }
    }

    /// The toggle only asks; the setting changes once the user agrees.
    #[template_callback]
    fn on_conversation_toggled(&self) {
        let imp = self.imp();
        if imp.conversation_quiet.get() {
            return;
        }
        let wanted = imp.conversation_button.is_active();
        self.show_conversation_mode();
        if !wanted {
            self.set_conversation_mode(false);
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                if window.confirm_conversation_mode().await {
                    window.set_conversation_mode(true);
                    if window.imp().turn.borrow().is_none() {
                        window.request_listen();
                    }
                }
            }
        ));
    }

    async fn confirm_conversation_mode(&self) -> bool {
        let Some(app) = self.app() else {
            return false;
        };
        let settings = app.settings().clone();
        let imp = self.imp();
        let overrides = !imp.speaker_button.is_active() || !imp.voice_button.is_active();
        if !overrides || !settings.boolean("assist-conversation-notice") {
            return true;
        }
        let skip = gtk::CheckButton::with_label(&gettext("Don't show this message again"));
        skip.set_halign(gtk::Align::Center);
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Conversation Mode"))
            .body(gettext(
                "Enabling Conversation Mode will enable voice input and spoken responses.",
            ))
            .extra_child(&skip)
            .default_response("continue")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("continue", &gettext("_Continue")),
        ]);
        dialog.set_response_appearance("continue", adw::ResponseAppearance::Suggested);
        let agreed = dialog.choose_future(Some(self)).await == "continue";
        if agreed && skip.is_active() {
            let _ = settings.set_boolean("assist-conversation-notice", false);
        }
        agreed
    }

    fn conversation_mode(&self) -> bool {
        self.app()
            .is_some_and(|app| app.settings().boolean(CONVERSATION_KEY))
    }

    fn is_dismissal(&self, text: &str) -> bool {
        let Some(app) = self.app() else {
            return false;
        };
        let settings = app.settings();
        let phrases: Vec<String> = settings
            .strv("assist-dismissal-phrases")
            .iter()
            .map(ToString::to_string)
            .collect();
        settings.boolean("assist-dismissal") && assist::is_farewell(text, &phrases)
    }

    fn expect_follow_up(&self) {
        if self.conversation_mode() {
            self.imp().follow_up.set(true);
        }
    }

    /// Listens again, without the tone, once a reply in conversation mode
    /// has finished. A turn that heard nothing gets no reply, which ends the
    /// conversation.
    fn maybe_follow_up(&self) {
        let imp = self.imp();
        if !imp.follow_up.get() || imp.reply_audio.get() {
            return;
        }
        imp.follow_up.set(false);
        if imp.closing.take() {
            self.close();
            return;
        }
        if self.conversation_mode()
            && self.is_visible()
            && imp.ready.get()
            && imp.turn.borrow().is_none()
        {
            self.start_turn(false);
        }
    }

    fn update_speaker_icon(&self) {
        let button = &self.imp().speaker_button;
        button.set_icon_name(if button.is_active() {
            "audio-volume-high-symbolic"
        } else {
            "audio-volume-muted-symbolic"
        });
    }

    /// An assistant without text-to-speech fails a run that asks for it.
    fn speaks_replies(&self) -> bool {
        let imp = self.imp();
        imp.speaker_button.is_active()
            && imp
                .pipelines
                .borrow()
                .resolve(imp.pipeline_id.borrow().as_deref())
                .is_some_and(|pipeline| pipeline.tts_engine.is_some())
    }

    fn ask(&self, text: String) {
        let Some(app) = self.app() else {
            return;
        };
        let client = app.client().clone();
        let pipeline = self.imp().pipeline_id.borrow().clone();
        if let Some(id) = &pipeline {
            self.remember_pipeline(id);
        }
        let speak_reply = self.speaks_replies();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                let conversation_id = window.imp().conversation_id.borrow().clone();
                let run = assist::ask(
                    &client,
                    &text,
                    pipeline.as_deref(),
                    conversation_id.as_deref(),
                    speak_reply,
                )
                .await;
                let mut subscription = match run {
                    Ok(subscription) => subscription,
                    Err(err) => {
                        window.add_bubble(&error_text(&err), Speaker::Error);
                        return;
                    }
                };
                while let Some(event) = subscription.events.next().await {
                    match assist::parse_voice_event(&event) {
                        Some(VoiceEvent::Replied(reply)) => {
                            window.show_reply(reply);
                            window.expect_follow_up();
                        }
                        Some(VoiceEvent::Audio(url)) if window.speaks_replies() => {
                            window.play_reply(url)
                        }
                        Some(VoiceEvent::Failed { code, message }) => {
                            window.add_bubble(
                                if message.is_empty() { &code } else { &message },
                                Speaker::Error,
                            );
                        }
                        Some(VoiceEvent::Finished) => break,
                        _ => {}
                    }
                }
                window.maybe_follow_up();
            }
        ));
    }

    fn show_reply(&self, reply: assist::Reply) {
        if reply.conversation_id.is_some() {
            self.imp().conversation_id.replace(reply.conversation_id);
        }
        let speech = if reply.speech.is_empty() {
            gettext("Done")
        } else {
            reply.speech
        };
        self.add_bubble(&speech, Speaker::Assist);
    }

    /// Called each time the window is shown, including by the assistant
    /// button; listens when voice or conversation mode is on in Preferences.
    /// `voice` listens even when the preference is off.
    pub fn listen_on_open(&self, voice: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let settings = app.settings();
        if (voice || settings.boolean("use-voice-assistant") || settings.boolean(CONVERSATION_KEY))
            && self.imp().turn.borrow().is_none()
        {
            self.request_listen();
        }
    }

    #[template_callback]
    fn on_voice_toggled(&self) {
        let imp = self.imp();
        if imp.voice_quiet.get() {
            return;
        }
        if imp.voice_button.is_active() {
            self.request_listen();
        } else {
            imp.listen_when_ready.set(false);
            self.cancel_turn();
            self.set_conversation_mode(false);
        }
    }

    fn set_voice_active(&self, active: bool) {
        let imp = self.imp();
        imp.voice_quiet.set(true);
        imp.voice_button.set_active(active);
        imp.voice_quiet.set(false);
    }

    fn show_voice_status(&self, text: Option<&str>, listening: bool) {
        let imp = self.imp();
        if let Some(text) = text {
            imp.voice_label.set_label(text);
        }
        imp.voice_revealer.set_reveal_child(text.is_some());
        if listening {
            imp.voice_button.add_css_class("listening");
        } else {
            imp.voice_button.remove_css_class("listening");
        }
    }

    fn request_listen(&self) {
        let imp = self.imp();
        if imp.ready.get() {
            self.start_turn(true);
        } else {
            imp.listen_when_ready.set(true);
            self.set_voice_active(true);
            self.show_voice_status(Some(&gettext("Waiting for Home Assistant…")), false);
        }
    }

    fn voice_unavailable(&self) -> Option<String> {
        let imp = self.imp();
        let no_stt = imp
            .pipelines
            .borrow()
            .resolve(imp.pipeline_id.borrow().as_deref())
            .is_some_and(|pipeline| pipeline.stt_engine.is_none());
        if no_stt {
            return Some(gettext(
                "This assistant cannot listen. Choose one with speech-to-text in Home Assistant.",
            ));
        }
        if !voice::has_microphone() {
            return Some(gettext("No microphone is available"));
        }
        None
    }

    /// `tone` plays the listening sound first.
    fn start_turn(&self, tone: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let imp = self.imp();
        if let Some(reason) = self.voice_unavailable() {
            self.set_conversation_mode(false);
            self.set_voice_active(false);
            self.show_voice_status(None, false);
            self.add_bubble(&reason, Speaker::Note);
            return;
        }
        voice::stop_playback();
        imp.follow_up.set(false);
        imp.reply_audio.set(false);
        imp.closing.set(false);
        let tone_millis = if tone {
            voice::listening_tone(&app.settings().string("listening-tone"))
        } else {
            0
        };
        let (recorder, mut audio) = match voice::Recorder::start(assist::SAMPLE_RATE) {
            Ok(started) => started,
            Err(err) => {
                self.set_voice_active(false);
                self.show_voice_status(None, false);
                self.add_bubble(
                    &format!("{}: {err}", gettext("Could not use the microphone")),
                    Speaker::Error,
                );
                return;
            }
        };
        let generation = imp.turn_generation.get() + 1;
        imp.turn_generation.set(generation);
        let timeout = glib::timeout_add_seconds_local_once(
            MAX_LISTEN_SECONDS,
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                move || {
                    if let Some(turn) = window.imp().turn.borrow_mut().as_mut() {
                        turn.timeout.take();
                    }
                    window.end_audio(generation);
                }
            ),
        );
        imp.turn.replace(Some(Turn {
            recorder: Some(recorder),
            timeout: Some(timeout),
            skip: (tone_millis * u64::from(assist::SAMPLE_RATE) / 1000 * 2) as usize,
            ..Turn::default()
        }));
        self.set_voice_active(true);
        self.show_voice_status(Some(&gettext("Listening…")), true);

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                while let Some(captured) = audio.next().await {
                    if window.imp().turn_generation.get() != generation {
                        return;
                    }
                    match captured {
                        voice::Captured::Audio(chunk) => window.feed(&chunk),
                        voice::Captured::Failed(err) => {
                            window.add_bubble(
                                &format!("{}: {err}", gettext("Could not use the microphone")),
                                Speaker::Error,
                            );
                            window.cancel_turn();
                            return;
                        }
                    }
                }
            }
        ));

        let client = app.client().clone();
        let pipeline = imp.pipeline_id.borrow().clone();
        let conversation_id = imp.conversation_id.borrow().clone();
        let speak_reply = self.speaks_replies();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                let run = assist::listen(
                    &client,
                    pipeline.as_deref(),
                    conversation_id.as_deref(),
                    speak_reply,
                )
                .await;
                let mut subscription = match run {
                    Ok(subscription) => subscription,
                    Err(err) => {
                        window.add_bubble(&error_text(&err), Speaker::Error);
                        window.cancel_turn();
                        return;
                    }
                };
                if let Some(turn) = window.imp().turn.borrow_mut().as_mut() {
                    turn.subscription = Some(subscription.id);
                }
                while let Some(event) = subscription.events.next().await {
                    if window.imp().turn_generation.get() != generation {
                        return;
                    }
                    if let Some(event) = assist::parse_voice_event(&event) {
                        let finished = event == VoiceEvent::Finished;
                        window.on_voice_event(event, generation);
                        if finished {
                            break;
                        }
                    }
                }
                window.finish_turn(generation);
                window.maybe_follow_up();
            }
        ));
    }

    fn send_frame(&self, handler: u8, chunk: &[u8]) {
        let Some(app) = self.app() else {
            return;
        };
        let mut frame = Vec::with_capacity(chunk.len() + 1);
        frame.push(handler);
        frame.extend_from_slice(chunk);
        if let Err(err) = app.client().send_binary(&frame) {
            log::debug!("Audio frame dropped: {err}");
        }
    }

    fn feed(&self, chunk: &[u8]) {
        let mut turn = self.imp().turn.borrow_mut();
        let Some(turn) = turn.as_mut().filter(|turn| !turn.audio_ended) else {
            return;
        };
        let skipped = turn.skip.min(chunk.len());
        turn.skip -= skipped;
        let chunk = &chunk[skipped..];
        if chunk.is_empty() {
            return;
        }
        match turn.handler {
            Some(handler) => self.send_frame(handler, chunk),
            None => turn.buffered.extend_from_slice(chunk),
        }
    }

    /// Stops the microphone and tells the server the audio is complete; an
    /// empty frame with the handler byte alone marks the end.
    fn end_audio(&self, generation: u64) {
        let imp = self.imp();
        if imp.turn_generation.get() != generation {
            return;
        }
        {
            let mut turn = imp.turn.borrow_mut();
            let Some(turn) = turn.as_mut() else {
                return;
            };
            turn.recorder.take();
            if let Some(source) = turn.timeout.take() {
                source.remove();
            }
            if !turn.audio_ended {
                turn.audio_ended = true;
                if let Some(handler) = turn.handler {
                    self.send_frame(handler, &[]);
                }
            }
        }
        self.show_voice_status(Some(&gettext("Thinking…")), false);
    }

    fn on_voice_event(&self, event: VoiceEvent, generation: u64) {
        let imp = self.imp();
        match event {
            VoiceEvent::Started { handler } => {
                let mut turn = imp.turn.borrow_mut();
                if let Some(turn) = turn.as_mut() {
                    turn.handler = Some(handler);
                    let buffered = std::mem::take(&mut turn.buffered);
                    if !buffered.is_empty() {
                        self.send_frame(handler, &buffered);
                    }
                    // The cap can expire before the run starts.
                    if turn.audio_ended {
                        self.send_frame(handler, &[]);
                    }
                }
            }
            VoiceEvent::SpeechEnded => self.end_audio(generation),
            VoiceEvent::Heard(text) => {
                self.end_audio(generation);
                if self.conversation_mode() && self.is_dismissal(&text) {
                    imp.closing.set(true);
                }
                if !text.is_empty() {
                    self.add_bubble(&text, Speaker::User);
                }
            }
            VoiceEvent::Replied(reply) => {
                self.show_reply(reply);
                self.expect_follow_up();
            }
            VoiceEvent::Audio(url) if self.speaks_replies() => self.play_reply(url),
            VoiceEvent::Audio(_) => {}
            VoiceEvent::Failed { code, message } => {
                self.end_audio(generation);
                let text = if code == "stt-no-text-recognized" {
                    gettext("Sorry, I didn't catch that")
                } else if message.is_empty() {
                    code
                } else {
                    message
                };
                self.add_bubble(&text, Speaker::Error);
            }
            VoiceEvent::Finished => {}
        }
    }

    fn play_reply(&self, url: String) {
        let Some(app) = self.app() else {
            return;
        };
        let client = app.client().clone();
        let window = self.downgrade();
        self.imp().reply_audio.set(true);
        glib::spawn_future_local(async move {
            let full = if url.starts_with('/') {
                format!("{}{url}", client.active_url().trim_end_matches('/'))
            } else {
                url
            };
            let response = ha::http_get(&full, None).await;
            let Some(shown) = window.upgrade() else {
                return;
            };
            match response {
                // Muting can happen while the audio downloads.
                Ok(_) if !shown.speaks_replies() => {}
                Ok(response) if response.is_success() => {
                    let done = shown.downgrade();
                    let played = voice::play(&response.body, move |finished| {
                        let Some(window) = done.upgrade() else {
                            return;
                        };
                        window.imp().speaker_button.remove_css_class("speaking");
                        if window.imp().turn.borrow().is_none() {
                            window.show_voice_status(None, false);
                        }
                        if finished {
                            window.imp().reply_audio.set(false);
                            window.maybe_follow_up();
                        }
                    });
                    match played {
                        Ok(()) => {
                            shown.imp().speaker_button.add_css_class("speaking");
                            shown.show_voice_status(Some(&gettext("Speaking…")), false);
                            return;
                        }
                        Err(err) => log::warn!("Could not play the reply: {err}"),
                    }
                }
                Ok(response) => log::warn!("Reply audio failed: HTTP {}", response.status),
                Err(err) => log::warn!("Reply audio failed: {err}"),
            }
            shown.imp().reply_audio.set(false);
            if shown.imp().turn.borrow().is_none() {
                shown.show_voice_status(None, false);
            }
            shown.maybe_follow_up();
        });
    }

    fn finish_turn(&self, generation: u64) {
        let imp = self.imp();
        if imp.turn_generation.get() != generation {
            return;
        }
        if let Some(mut turn) = imp.turn.take()
            && let Some(source) = turn.timeout.take()
        {
            source.remove();
        }
        self.set_voice_active(false);
        // A spoken reply still downloading shows the Speaking status next.
        if !imp.reply_audio.get() {
            self.show_voice_status(None, false);
        }
    }

    /// Stops listening without acting on anything said; the server run is
    /// cancelled too.
    pub fn cancel_turn(&self) {
        let imp = self.imp();
        imp.turn_generation.set(imp.turn_generation.get() + 1);
        imp.listen_when_ready.set(false);
        imp.follow_up.set(false);
        imp.reply_audio.set(false);
        imp.closing.set(false);
        voice::stop_playback();
        if let Some(mut turn) = imp.turn.take() {
            if let Some(source) = turn.timeout.take() {
                source.remove();
            }
            if let (Some(id), Some(app)) = (turn.subscription, self.app()) {
                let client = app.client().clone();
                glib::spawn_future_local(async move { client.unsubscribe(id).await });
            }
        }
        self.set_voice_active(false);
        self.show_voice_status(None, false);
    }

    fn add_bubble(&self, text: &str, speaker: Speaker) {
        let imp = self.imp();
        let label = gtk::Label::builder()
            .label(text)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .selectable(true)
            .build();
        let (classes, align): (&[&str], _) = match speaker {
            Speaker::User => (&["assist-bubble", "user"], gtk::Align::End),
            Speaker::Assist => (&["assist-bubble", "reply"], gtk::Align::Start),
            Speaker::Error => (&["assist-bubble", "error"], gtk::Align::Start),
            Speaker::Note => (&["assist-note", "dim-label", "caption"], gtk::Align::Center),
        };
        for class in classes {
            label.add_css_class(class);
        }
        label.set_halign(align);
        imp.transcript.append(&label);

        // The new height is known only after the next layout pass.
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move || {
                let adjustment = window.imp().scroller.vadjustment();
                adjustment.set_value(adjustment.upper() - adjustment.page_size());
            }
        ));
    }
}
