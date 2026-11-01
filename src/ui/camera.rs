//! Live camera frames for the code scanner, through the camera portal: the
//! only way a Flatpak app reaches the camera without broad device access.
//! The portal hands over a PipeWire connection that `pipewiresrc` uses.
//!
//! State changes block while the camera starts, so they run on worker
//! threads. Stopping the pipeline from its streaming thread deadlocks.

use std::collections::HashMap;
use std::os::fd::{IntoRawFd, OwnedFd};

use futures_util::StreamExt;
use gst::prelude::*;
use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const CAMERA: &str = "org.freedesktop.portal.Camera";

pub enum Source {
    Portal(OwnedFd),
    /// A GStreamer source description in `SANCTUM_CAMERA_SOURCE`, which
    /// replaces the camera for testing on machines without one.
    Override(String),
}

/// Asks for camera access, which the portal remembers once granted, and
/// returns a PipeWire connection limited to the cameras. The portal's
/// `IsCameraPresent` is not consulted: it reads false on FuriOS although
/// portal-using apps there do get the camera.
pub async fn open() -> Result<Source, String> {
    if let Some(source) = std::env::var("SANCTUM_CAMERA_SOURCE")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(Source::Override(source));
    }
    let connection = gio::bus_get_future(gio::BusType::Session)
        .await
        .map_err(|err| err.to_string())?;
    let sender = connection
        .unique_name()
        .map(|name| name.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let token = format!("sanctum{}", glib::random_int_range(0, i32::MAX));
    let handle = format!("{PORTAL_PATH}/request/{sender}/{token}");
    // Subscribed before the call so a fast response is not missed.
    let mut responses = connection
        .receive_signal_parameters::<(u32, HashMap<String, glib::Variant>)>(
            Some(PORTAL_BUS),
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(&handle),
            None,
            gio::DBusSignalFlags::NONE,
        );
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", &token);
    connection
        .call_future(
            Some(PORTAL_BUS),
            PORTAL_PATH,
            CAMERA,
            "AccessCamera",
            Some(&glib::Variant::tuple_from_iter([options.end()])),
            None,
            gio::DBusCallFlags::NONE,
            -1,
        )
        .await
        .map_err(|err| err.to_string())?;
    match responses.next().await {
        Some(Ok((0, _))) => {}
        Some(Ok(_)) => return Err("camera access was not allowed".to_owned()),
        _ => return Err("no answer from the camera portal".to_owned()),
    }
    let (reply, fds) = connection
        .call_with_unix_fd_list_future(
            Some(PORTAL_BUS),
            PORTAL_PATH,
            CAMERA,
            "OpenPipeWireRemote",
            Some(&glib::Variant::tuple_from_iter([glib::VariantDict::new(
                None,
            )
            .end()])),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::UnixFDList::NONE,
        )
        .await
        .map_err(|err| err.to_string())?;
    let index = reply
        .child_value(0)
        .get::<glib::variant::Handle>()
        .ok_or("no PipeWire connection")?
        .0;
    let fds = fds.ok_or("no PipeWire connection")?;
    fds.get(index)
        .map(Source::Portal)
        .map_err(|err| err.to_string())
}

/// The preview goes to `preview`, a `gtk4paintablesink`, and greyscale
/// frames for decoding to the `scan` appsink. Asking the camera for a
/// modest size first keeps conversion cheap on a phone; some cameras offer
/// nothing in that range, so the second attempt takes any size.
fn description(source: &str, modest: bool) -> String {
    let size = if modest {
        " ! video/x-raw,width=[320,1280],height=[240,1280]"
    } else {
        ""
    };
    format!(
        "{source}{size} ! videoconvert ! videoscale ! \
         video/x-raw,width=640,pixel-aspect-ratio=1/1 ! tee name=t \
         t. ! queue leaky=downstream max-size-buffers=2 ! \
         gtk4paintablesink name=preview \
         t. ! queue leaky=downstream max-size-buffers=1 ! videoconvert ! \
         video/x-raw,format=GRAY8 ! appsink name=scan max-buffers=1 drop=true sync=false"
    )
}

/// Builds and starts the pipeline. Blocks while the camera starts, so it
/// runs on a worker thread.
pub fn start(source: Source) -> Result<gst::Pipeline, String> {
    gst::init().map_err(|err| err.to_string())?;
    let mut last = String::new();
    for modest in [true, false] {
        let element = match &source {
            // pipewiresrc takes the descriptor as a number, so each attempt
            // gets a copy.
            Source::Portal(fd) => format!(
                "pipewiresrc fd={} always-copy=true",
                fd.try_clone().map_err(|err| err.to_string())?.into_raw_fd()
            ),
            Source::Override(description) => description.clone(),
        };
        let pipeline = gst::parse::launch(&description(&element, modest))
            .map_err(|err| err.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "unexpected pipeline type".to_owned())?;
        if pipeline.set_state(gst::State::Playing).is_ok()
            && pipeline.state(gst::ClockTime::from_seconds(10)).0.is_ok()
        {
            return Ok(pipeline);
        }
        last = "the camera could not be started".to_owned();
        let _ = pipeline.set_state(gst::State::Null);
    }
    Err(last)
}

/// Stops the pipeline on a separate thread, since stopping waits for the
/// camera and must not run on the pipeline's streaming threads.
pub fn stop(pipeline: gst::Pipeline) {
    std::thread::spawn(move || {
        let _ = pipeline.set_state(gst::State::Null);
    });
}

/// Calls `on_frame` with greyscale frames on GStreamer's streaming thread.
/// Frames that arrive while one is handled are dropped.
pub fn on_frames(
    pipeline: &gst::Pipeline,
    on_frame: impl Fn(&[u8], u32, u32) + Send + Sync + 'static,
) -> bool {
    let Some(sink) = pipeline
        .by_name("scan")
        .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
    else {
        return false;
    };
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let size = sample.caps().and_then(|caps| {
                    let structure = caps.structure(0)?;
                    Some((
                        structure.get::<i32>("width").ok()?,
                        structure.get::<i32>("height").ok()?,
                    ))
                });
                if let (Some(buffer), Some((width, height))) = (sample.buffer(), size)
                    && let Ok(map) = buffer.map_readable()
                {
                    let (width, height) = (width as u32, height as u32);
                    // GStreamer pads greyscale rows to four bytes.
                    let stride = width.div_ceil(4) * 4;
                    let data = map.as_slice();
                    if stride == width {
                        on_frame(data, width, height);
                    } else {
                        let packed: Vec<u8> = data
                            .chunks(stride as usize)
                            .take(height as usize)
                            .flat_map(|row| &row[..width as usize])
                            .copied()
                            .collect();
                        on_frame(&packed, width, height);
                    }
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    true
}

pub fn paintable(pipeline: &gst::Pipeline) -> Option<gtk::gdk::Paintable> {
    pipeline
        .by_name("preview")
        .map(|sink| sink.property::<gtk::gdk::Paintable>("paintable"))
}

/// Restarts a paused pipeline on a separate thread, like `pause`.
pub fn play(pipeline: gst::Pipeline) {
    std::thread::spawn(move || {
        let _ = pipeline.set_state(gst::State::Playing);
    });
}

/// Freezes the preview on its last frame, on a separate thread for the
/// same reason as `stop`.
pub fn pause(pipeline: gst::Pipeline) {
    std::thread::spawn(move || {
        let _ = pipeline.set_state(gst::State::Paused);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_pipelines() {
        assert!(description("pipewiresrc fd=7", true).starts_with("pipewiresrc fd=7 ! "));
        assert!(!description("pipewiresrc", false).contains("width=[320"));
    }
}
