//! Microphone capture and reply playback with GStreamer. GStreamer is set up
//! when voice is first used, which keeps text-only Assist quick to start.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use futures_channel::mpsc;
use gst::prelude::*;
use gtk::glib;

type PlaybackDone = Box<dyn FnOnce(bool)>;

thread_local! {
    static PLAYER: RefCell<Option<gst::Element>> = const { RefCell::new(None) };
    static PLAYED: RefCell<Option<PlaybackDone>> = const { RefCell::new(None) };
    static TONE: RefCell<Option<gst::Element>> = const { RefCell::new(None) };
    static MICROPHONE: Cell<Option<bool>> = const { Cell::new(None) };
}

fn init() -> Result<(), String> {
    gst::init().map_err(|err| err.to_string())
}

/// Whether any audio input is visible. Inside Flatpak this needs the
/// PulseAudio socket; without it no source shows up. Cached after the first
/// check, which takes a moment while the device providers start.
pub fn has_microphone() -> bool {
    if let Some(known) = MICROPHONE.get() {
        return known;
    }
    if source_override().is_some() {
        return true;
    }
    let found = init().is_ok() && {
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Audio/Source"), None);
        let started = monitor.start().is_ok();
        let found = started && !monitor.devices().is_empty();
        if started {
            monitor.stop();
        }
        found
    };
    MICROPHONE.set(Some(found));
    found
}

/// A GStreamer source description in `SANCTUM_AUDIO_SOURCE`, such as
/// `audiotestsrc is-live=true`, replaces the microphone for testing on
/// machines without audio input.
fn source_override() -> Option<String> {
    std::env::var("SANCTUM_AUDIO_SOURCE")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub enum Captured {
    /// 16-bit little-endian mono PCM at `assist::SAMPLE_RATE`.
    Audio(Vec<u8>),
    Failed(String),
}

/// Records until dropped or stopped. Chunks arrive on the main thread through
/// the returned receiver, since GStreamer delivers them on another thread.
#[derive(Debug)]
pub struct Recorder {
    pipeline: gst::Pipeline,
}

impl Recorder {
    pub fn start(sample_rate: u32) -> Result<(Self, mpsc::UnboundedReceiver<Captured>), String> {
        init()?;
        let source = source_override().unwrap_or_else(|| "autoaudiosrc".to_owned());
        let description = format!(
            "{source} ! audioconvert ! audioresample ! \
             audio/x-raw,format=S16LE,channels=1,rate={sample_rate},layout=interleaved ! \
             appsink name=sink sync=false emit-signals=false"
        );
        let pipeline = gst::parse::launch(&description)
            .map_err(|err| err.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "unexpected pipeline type".to_owned())?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| "no audio sink".to_owned())?;

        let (sender, receiver) = mpsc::unbounded();
        let audio = sender.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(buffer) = sample.buffer()
                        && let Ok(map) = buffer.map_readable()
                    {
                        let _ = audio.unbounded_send(Captured::Audio(map.as_slice().to_vec()));
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        let bus = pipeline.bus().ok_or_else(|| "no bus".to_owned())?;
        bus.set_sync_handler(move |_, message| {
            if let gst::MessageView::Error(error) = message.view() {
                let _ = sender.unbounded_send(Captured::Failed(error.error().to_string()));
            }
            gst::BusSyncReply::Drop
        });
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| "the microphone could not be opened".to_owned())?;
        Ok((Self { pipeline }, receiver))
    }

    pub fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn cache_file() -> PathBuf {
    glib::user_cache_dir().join("sanctum").join("reply-audio")
}

/// Plays a spoken reply, replacing any reply still playing. `on_done` runs
/// once playback ends, with `true` only when the reply played to the end.
pub fn play(bytes: &[u8], on_done: impl FnOnce(bool) + 'static) -> Result<(), String> {
    init()?;
    stop_playback();
    let path = cache_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    }
    std::fs::write(&path, bytes).map_err(|err| err.to_string())?;
    let uri = glib::filename_to_uri(&path, None).map_err(|err| err.to_string())?;
    let player = gst::ElementFactory::make("playbin")
        .property("uri", uri.as_str())
        .build()
        .map_err(|err| err.to_string())?;
    if let Some(bus) = player.bus() {
        // The element is released at EOS or error.
        bus.set_sync_handler(|_, message| {
            match message.view() {
                gst::MessageView::Eos(_) => {
                    glib::MainContext::default().invoke(|| end_playback(true));
                }
                gst::MessageView::Error(_) => {
                    glib::MainContext::default().invoke(|| end_playback(false));
                }
                _ => {}
            }
            gst::BusSyncReply::Drop
        });
    }
    player
        .set_state(gst::State::Playing)
        .map_err(|_| "the reply could not be played".to_owned())?;
    PLAYER.with(|cell| cell.replace(Some(player)));
    PLAYED.with(|cell| cell.replace(Some(Box::new(on_done))));
    Ok(())
}

pub fn stop_playback() {
    end_playback(false);
}

fn end_playback(finished: bool) {
    PLAYER.with(|cell| {
        if let Some(player) = cell.take() {
            let _ = player.set_state(gst::State::Null);
        }
    });
    if let Some(done) = PLAYED.with(RefCell::take) {
        done(finished);
    }
}

/// How long the listening tone stays audible; capture skips this much so the
/// tone is not transcribed.
const TONE_MILLIS: u64 = 240;

/// Plays the listening tone unless `name` is "none", and returns how many
/// milliseconds of capture to skip. Choices from earlier versions play the
/// current tone. Failures are logged; Assist works without the tone.
pub fn listening_tone(name: &str) -> u64 {
    if name == "none" {
        return 0;
    }
    match play_tone("prompt") {
        Ok(()) => TONE_MILLIS,
        Err(err) => {
            log::debug!("Could not play the listening tone: {err}");
            0
        }
    }
}

fn play_tone(name: &str) -> Result<(), String> {
    init()?;
    stop_tone();
    let uri = format!("resource:///net/dunevent/Sanctum/sounds/listening-{name}.flac");
    let player = gst::ElementFactory::make("playbin")
        .property("uri", uri.as_str())
        .build()
        .map_err(|err| err.to_string())?;
    if let Some(bus) = player.bus() {
        bus.set_sync_handler(|_, message| {
            if matches!(
                message.view(),
                gst::MessageView::Eos(_) | gst::MessageView::Error(_)
            ) {
                glib::MainContext::default().invoke(stop_tone);
            }
            gst::BusSyncReply::Drop
        });
    }
    player
        .set_state(gst::State::Playing)
        .map_err(|_| "the listening tone could not be played".to_owned())?;
    TONE.with(|cell| cell.replace(Some(player)));
    Ok(())
}

fn stop_tone() {
    TONE.with(|cell| {
        if let Some(player) = cell.take() {
            let _ = player.set_state(gst::State::Null);
        }
    });
}
