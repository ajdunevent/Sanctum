//! Code shared by the window app and `sanctum-service`. Nothing here links
//! GTK or WebKit, which keeps the service small.

pub mod agent;
pub mod background;
#[rustfmt::skip]
#[allow(dead_code)]
pub mod config;
pub mod control;
pub mod ha;
pub mod model;
pub mod notify;
pub mod reregister;
pub mod server_settings;
pub mod service;
pub mod voice;

use gettextrs::LocaleCategory;

use self::config::{GETTEXT_PACKAGE, LOCALEDIR};

pub fn init_gettext() {
    gettextrs::setlocale(LocaleCategory::LcAll, "");
    gettextrs::bindtextdomain(GETTEXT_PACKAGE, LOCALEDIR).expect("Unable to bind the text domain");
    gettextrs::bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8")
        .expect("Unable to set the text domain encoding");
    gettextrs::textdomain(GETTEXT_PACKAGE).expect("Unable to switch to the text domain");
}

/// Forwards the `log` crate to GLib logging under the `sanctum` domain. Info and
/// debug output appear with `G_MESSAGES_DEBUG=sanctum`.
pub fn init_logging() {
    struct GlibLogger(log::LevelFilter);

    impl log::Log for GlibLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= self.0
        }

        fn log(&self, record: &log::Record) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let level = match record.level() {
                log::Level::Error => glib::LogLevel::Critical,
                log::Level::Warn => glib::LogLevel::Warning,
                log::Level::Info => glib::LogLevel::Info,
                log::Level::Debug | log::Level::Trace => glib::LogLevel::Debug,
            };
            glib::g_log!("sanctum", level, "{}", record.args());
        }

        fn flush(&self) {}
    }

    let filter = if config::PROFILE == "Devel" {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    if log::set_boxed_logger(Box::new(GlibLogger(filter))).is_ok() {
        log::set_max_level(filter);
    }
}
