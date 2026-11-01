//! Reads barcodes and QR codes for the frontend, which asks the app to scan
//! when adding devices such as Z-Wave SmartStart ones. The camera comes
//! through the camera portal; a photo of the code works where it does not.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib};
use rxing::{BarcodeFormat, MultiFormatReader, Reader};

use super::camera;
use super::rotated::RotatedPaintable;

pub struct Request {
    pub title: String,
    pub description: String,
    pub alternative: Option<String>,
}

pub enum Outcome {
    Found { raw: String, format: &'static str },
    Canceled,
    Alternative,
}

/// The format names of the Barcode Detection API, which the frontend uses.
fn format_name(format: &BarcodeFormat) -> &'static str {
    match format {
        BarcodeFormat::QR_CODE => "qr_code",
        BarcodeFormat::AZTEC => "aztec",
        BarcodeFormat::CODE_128 => "code_128",
        BarcodeFormat::CODE_39 => "code_39",
        BarcodeFormat::CODE_93 => "code_93",
        BarcodeFormat::CODABAR => "codabar",
        BarcodeFormat::DATA_MATRIX => "data_matrix",
        BarcodeFormat::EAN_13 => "ean_13",
        BarcodeFormat::EAN_8 => "ean_8",
        BarcodeFormat::ITF => "itf",
        BarcodeFormat::PDF_417 => "pdf417",
        BarcodeFormat::UPC_A => "upc_a",
        BarcodeFormat::UPC_E => "upc_e",
        _ => "unknown",
    }
}

fn try_decode(luma: &[u8], width: u32, height: u32) -> Option<(String, &'static str)> {
    let source = rxing::Luma8LuminanceSource::new(luma.to_vec(), width, height).ok()?;
    let mut bitmap = rxing::BinaryBitmap::new(rxing::common::HybridBinarizer::new(source));
    let result = MultiFormatReader::default().decode(&mut bitmap).ok()?;
    Some((
        result.getText().to_owned(),
        format_name(result.getBarcodeFormat()),
    ))
}

fn rotate_clockwise(luma: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0; w * h];
    for y in 0..h {
        for x in 0..w {
            out[x * h + (h - 1 - y)] = luma[y * w + x];
        }
    }
    out
}

/// Tries `turns` quarter turns of the image.
fn decode_turns(
    luma: &[u8],
    width: u32,
    height: u32,
    turns: u32,
) -> Option<(String, &'static str)> {
    let mut frame = luma.to_vec();
    let (mut w, mut h) = (width, height);
    for turn in 0..turns {
        if let Some(found) = try_decode(&frame, w, h) {
            return Some(found);
        }
        if turn + 1 == turns {
            break;
        }
        frame = rotate_clockwise(&frame, w, h);
        (w, h) = (h, w);
    }
    None
}

/// Averages blocks of pixels so the long side is at most `limit`.
fn shrink(luma: &[u8], width: u32, height: u32, limit: u32) -> (Vec<u8>, u32, u32) {
    let factor = width.max(height).div_ceil(limit).max(1);
    if factor == 1 {
        return (luma.to_vec(), width, height);
    }
    let (w, h) = (width / factor, height / factor);
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            for dy in 0..factor {
                let row = ((y * factor + dy) * width + x * factor) as usize;
                sum += luma[row..row + factor as usize]
                    .iter()
                    .map(|&value| u32::from(value))
                    .sum::<u32>();
            }
            out.push((sum / (factor * factor)) as u8);
        }
    }
    (out, w, h)
}

/// A photo's orientation tag is not applied, so all four turns are tried.
/// Phone photos are large, so a reduced copy is tried first and the full
/// photo only when that finds nothing.
fn decode_photo(bytes: glib::Bytes) -> Result<Option<(String, &'static str)>, String> {
    let texture = gdk::Texture::from_bytes(&bytes).map_err(|err| err.to_string())?;
    let (width, height) = (texture.width() as u32, texture.height() as u32);
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (pixels, stride) = downloader.download_bytes();
    let mut luma = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        let row = &pixels[y * stride..y * stride + width as usize * 4];
        luma.extend(row.chunks_exact(4).map(|p| {
            ((u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000) as u8
        }));
    }
    let (small, w, h) = shrink(&luma, width, height, 1600);
    if let Some(found) = decode_turns(&small, w, h, 4) {
        return Ok(Some(found));
    }
    if w == width {
        return Ok(None);
    }
    Ok(decode_turns(&luma, width, height, 4))
}

pub struct Scanner {
    dialog: adw::Dialog,
    viewer: gtk::Stack,
    picture: gtk::Picture,
    status: gtk::Label,
    choose: gtk::Button,
    rotate: gtk::Button,
    pipeline: RefCell<Option<gst::Pipeline>>,
    preview: RefCell<Option<RotatedPaintable>>,
    on_outcome: Rc<dyn Fn(Outcome)>,
    /// Set once the alternative was chosen, so closing is not reported as a
    /// cancellation too. A sent code does not count: the frontend closes the
    /// scanner itself when it accepts one.
    reported: Cell<bool>,
    /// Set while a found code waits for the frontend, which pauses decoding.
    waiting: Arc<AtomicBool>,
    /// The code last sent; the frontend rejected it if scanning resumes, so
    /// it is not sent again.
    sent: Arc<Mutex<Option<String>>>,
    resume: RefCell<Option<glib::SourceId>>,
    /// Set when the frontend closes the scanner, so closing is not reported
    /// back as a cancellation.
    closed_by_page: Cell<bool>,
}

impl Scanner {
    pub fn open(
        parent: &impl IsA<gtk::Widget>,
        request: Request,
        on_outcome: impl Fn(Outcome) + 'static,
    ) -> Rc<Self> {
        let starting = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::Center)
            .build();
        starting.append(
            &adw::Spinner::builder()
                .width_request(32)
                .height_request(32)
                .build(),
        );
        starting.append(
            &gtk::Label::builder()
                .label(gettext("Starting the camera…"))
                .css_classes(["dim-label"])
                .build(),
        );
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .vexpand(true)
            .build();
        picture.add_css_class("card");
        let photo = adw::StatusPage::builder()
            .icon_name("camera-photo-symbolic")
            .title(&request.title)
            .vexpand(true)
            .build();
        photo.add_css_class("compact");
        let viewer = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vexpand(true)
            .build();
        viewer.add_named(&starting, Some("starting"));
        viewer.add_named(&picture, Some("live"));
        viewer.add_named(&photo, Some("photo"));

        let description = gtk::Label::builder()
            .label(&request.description)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let status = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build();
        let choose = gtk::Button::builder()
            .label(gettext("_Choose Photo…"))
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(18)
            .margin_end(18)
            .margin_top(6)
            .margin_bottom(18)
            .build();
        column.append(&viewer);
        column.append(&description);
        column.append(&status);
        column.append(&choose);
        let alternative = request
            .alternative
            .as_deref()
            .filter(|label| !label.is_empty())
            .map(|label| {
                gtk::Button::builder()
                    .label(label)
                    .halign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build()
            });
        if let Some(button) = &alternative {
            column.append(button);
        }
        let rotate = gtk::Button::builder()
            .icon_name("object-rotate-right-symbolic")
            .tooltip_text(gettext("Rotate Preview"))
            .visible(false)
            .build();
        rotate.update_property(&[gtk::accessible::Property::Label(&gettext("Rotate Preview"))]);
        let header = adw::HeaderBar::new();
        header.pack_start(&rotate);
        let view = adw::ToolbarView::builder().content(&column).build();
        view.add_top_bar(&header);
        let dialog = adw::Dialog::builder()
            .title(&request.title)
            .content_width(420)
            .content_height(640)
            .child(&view)
            .build();

        let scanner = Rc::new(Self {
            dialog: dialog.clone(),
            viewer,
            picture,
            status,
            choose: choose.clone(),
            rotate: rotate.clone(),
            pipeline: RefCell::default(),
            preview: RefCell::default(),
            on_outcome: Rc::new(on_outcome),
            reported: Cell::new(false),
            waiting: Arc::default(),
            sent: Arc::default(),
            resume: RefCell::default(),
            closed_by_page: Cell::new(false),
        });
        dialog.connect_closed({
            let weak = Rc::downgrade(&scanner);
            move |_| {
                let Some(scanner) = weak.upgrade() else {
                    return;
                };
                scanner.stop_camera();
                if let Some(timer) = scanner.resume.take() {
                    timer.remove();
                }
                if !scanner.closed_by_page.get() && !scanner.reported.get() {
                    (scanner.on_outcome)(Outcome::Canceled);
                }
            }
        });
        if let Some(button) = alternative {
            let weak = Rc::downgrade(&scanner);
            button.connect_clicked(move |_| {
                if let Some(scanner) = weak.upgrade() {
                    scanner.reported.set(true);
                    scanner.dialog.force_close();
                    (scanner.on_outcome)(Outcome::Alternative);
                }
            });
        }
        choose.connect_clicked({
            let weak = Rc::downgrade(&scanner);
            move |_| {
                if let Some(scanner) = weak.upgrade() {
                    scanner.choose_photo();
                }
            }
        });
        rotate.connect_clicked({
            let weak = Rc::downgrade(&scanner);
            move |_| {
                if let Some(scanner) = weak.upgrade() {
                    scanner.rotate_preview();
                }
            }
        });
        dialog.present(Some(parent));
        scanner.start_camera();
        scanner
    }

    fn settings() -> Option<gio::Settings> {
        gio::Application::default()
            .and_downcast::<crate::application::Application>()
            .map(|app| app.settings().clone())
    }

    /// Clockwise quarter turns of the preview, which the decoder does not
    /// need since it tries both ways across.
    fn turns() -> u32 {
        Self::settings().map_or(0, |settings| settings.uint("scanner-turns") % 4)
    }

    fn rotate_preview(&self) {
        let next = (Self::turns() + 1) % 4;
        if let Some(settings) = Self::settings() {
            let _ = settings.set_uint("scanner-turns", next);
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.set_turns(next);
        }
    }

    fn show_photo_choice(&self, reason: Option<&str>) {
        self.viewer.set_visible_child_name("photo");
        self.rotate.set_visible(false);
        self.choose.add_css_class("suggested-action");
        let hint = gettext("Take a photo of the code with the Camera app, then choose it here");
        match reason {
            Some(reason) => self.status.set_label(&format!("{reason}. {hint}")),
            None => self.status.set_label(&hint),
        }
    }

    fn start_camera(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let started = match camera::open().await {
                Ok(source) => gio::spawn_blocking(move || camera::start(source))
                    .await
                    .unwrap_or_else(|_| Err("the camera stopped starting".to_owned())),
                Err(err) => Err(err),
            };
            let Some(scanner) = weak.upgrade() else {
                if let Ok(pipeline) = started {
                    camera::stop(pipeline);
                }
                return;
            };
            match started {
                Ok(pipeline) => scanner.show_live(pipeline),
                Err(err) => {
                    log::info!("Camera unavailable for scanning: {err}");
                    scanner.show_photo_choice(Some(&gettext("The camera is not available")));
                }
            }
        });
    }

    fn show_live(self: &Rc<Self>, pipeline: gst::Pipeline) {
        let (sender, mut found) = futures_channel::mpsc::unbounded();
        let waiting = self.waiting.clone();
        let sent = self.sent.clone();
        let attached = camera::on_frames(&pipeline, move |frame, width, height| {
            if waiting.load(Ordering::SeqCst) {
                return;
            }
            // Live frames try two turns only, which covers QR codes and
            // barcodes held either way across.
            let Some((raw, format)) = decode_turns(frame, width, height, 2) else {
                return;
            };
            if sent
                .lock()
                .is_ok_and(|sent| sent.as_deref() == Some(raw.as_str()))
            {
                return;
            }
            waiting.store(true, Ordering::SeqCst);
            let _ = sender.unbounded_send((raw, format));
        });
        let Some(paintable) = camera::paintable(&pipeline).filter(|_| attached) else {
            camera::stop(pipeline);
            self.show_photo_choice(Some(&gettext("The camera is not available")));
            return;
        };
        let preview = RotatedPaintable::new(&paintable, Self::turns());
        self.picture.set_paintable(Some(&preview));
        self.preview.replace(Some(preview));
        self.pipeline.replace(Some(pipeline));
        self.viewer.set_visible_child_name("live");
        self.rotate.set_visible(true);
        self.status
            .set_label(&gettext("Point the camera at the code"));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Some((raw, format)) = found.next().await {
                let Some(scanner) = weak.upgrade() else {
                    return;
                };
                // The last frame stays up while the frontend checks the code.
                if let Some(pipeline) = scanner.pipeline.borrow().as_ref() {
                    camera::pause(pipeline.clone());
                }
                if let Ok(mut sent) = scanner.sent.lock() {
                    *sent = Some(raw.clone());
                }
                scanner.found(raw, format);
                scanner.resume_later();
            }
        });
    }

    fn stop_camera(&self) {
        if let Some(pipeline) = self.pipeline.take() {
            camera::stop(pipeline);
        }
    }

    /// The frontend closes the scanner when it accepts a code, and may show
    /// a rejection on its page without telling the scanner, so scanning
    /// resumes after a moment.
    fn resume_later(self: &Rc<Self>) {
        if let Some(timer) = self.resume.take() {
            timer.remove();
        }
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_seconds_local_once(4, move || {
            if let Some(scanner) = weak.upgrade() {
                scanner.resume.take();
                scanner
                    .status
                    .set_label(&gettext("Point the camera at the code"));
                scanner.resume_scanning();
            }
        });
        self.resume.replace(Some(timer));
    }

    fn resume_scanning(&self) {
        let Some(pipeline) = self.pipeline.borrow().clone() else {
            return;
        };
        camera::play(pipeline);
        self.waiting.store(false, Ordering::SeqCst);
    }

    fn found(&self, raw: String, format: &'static str) {
        self.status.set_label(&gettext("Checking the code…"));
        (self.on_outcome)(Outcome::Found { raw, format });
    }

    fn choose_photo(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&gettext("Images")));
        filter.add_mime_type("image/*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title(gettext("Choose a Photo of the Code"))
            .filters(&filters)
            .default_filter(&filter)
            .build();
        let weak = Rc::downgrade(self);
        let window = self.dialog.root().and_downcast::<gtk::Window>();
        glib::spawn_future_local(async move {
            let Ok(file) = chooser.open_future(window.as_ref()).await else {
                return;
            };
            let Some(scanner) = weak.upgrade() else {
                return;
            };
            scanner.stop_camera();
            scanner.viewer.set_visible_child_name("photo");
            scanner.rotate.set_visible(false);
            scanner.choose.set_sensitive(false);
            scanner.status.set_label(&gettext("Reading the code…"));
            let decoded = match file.load_bytes_future().await {
                Ok((bytes, _)) => gio::spawn_blocking(move || decode_photo(bytes))
                    .await
                    .unwrap_or_else(|_| Err("decoding stopped".to_owned())),
                Err(err) => Err(err.to_string()),
            };
            let Some(scanner) = weak.upgrade() else {
                return;
            };
            scanner.choose.set_sensitive(true);
            match decoded {
                Ok(Some((raw, format))) => scanner.found(raw, format),
                Ok(None) => scanner.status.set_label(&gettext(
                    "No code was found in that photo. Try a closer, sharper photo.",
                )),
                Err(err) => {
                    log::warn!("Could not read the photo: {err}");
                    scanner
                        .status
                        .set_label(&gettext("That photo could not be opened"));
                }
            }
        });
    }

    /// A message from the frontend, such as a code of the wrong kind. The
    /// scanner stays open for another try.
    pub fn notify(&self, message: &str) {
        self.status.set_label(message);
        if let Some(timer) = self.resume.take() {
            timer.remove();
        }
        self.resume_scanning();
    }

    pub fn close(&self) {
        self.closed_by_page.set(true);
        self.dialog.force_close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_shrinks() {
        let frame = [1, 2, 3, 4, 5, 6];
        assert_eq!(rotate_clockwise(&frame, 3, 2), vec![4, 1, 5, 2, 6, 3]);
        let (small, w, h) = shrink(&[10, 20, 30, 40, 50, 60, 70, 80], 4, 2, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(small, vec![35, 55]);
    }
}
