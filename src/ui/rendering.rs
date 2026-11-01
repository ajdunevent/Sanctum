//! How WebKit draws pages. Some choices are environment variables that
//! WebKit reads when it starts a web process, so they are set before the
//! application starts any threads.

use std::cell::OnceCell;
use std::sync::OnceLock;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

use crate::config::APP_ID;

/// Paint tiles on CPU threads; compositing stays on the GPU.
const CPU_PAINTING: &str = "WEBKIT_SKIA_ENABLE_CPU_RENDERING";

/// Whether CPU painting was set up at start.
static STARTED: OnceLock<bool> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Gpu,
    Cpu,
    Software,
}

impl Mode {
    pub fn current(settings: &gio::Settings) -> Self {
        match settings.string("web-rendering").as_str() {
            "cpu" => Self::Cpu,
            "software" => Self::Software,
            _ => Self::Gpu,
        }
    }

    pub fn policy(self) -> webkit::HardwareAccelerationPolicy {
        match self {
            Self::Software => webkit::HardwareAccelerationPolicy::Never,
            Self::Gpu | Self::Cpu => webkit::HardwareAccelerationPolicy::Always,
        }
    }
}

/// Runs in `main` before GTK starts.
pub fn prepare_environment() {
    let settings = gio::Settings::new(APP_ID);
    if settings
        .user_value("web-hardware-acceleration")
        .and_then(|value| value.get::<bool>())
        == Some(false)
    {
        let _ = settings.set_string("web-rendering", "software");
        settings.reset("web-hardware-acceleration");
    }
    let cpu = Mode::current(&settings) == Mode::Cpu;
    if cpu {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var(CPU_PAINTING, "1") };
    }
    let _ = STARTED.set(cpu);
}

/// True when CPU painting differs from what the running web process was
/// started with.
pub fn needs_restart(settings: &gio::Settings) -> bool {
    STARTED.get().copied().unwrap_or_default() != (Mode::current(settings) == Mode::Cpu)
}

/// WebKit reads reduced motion from this where GTK has it (4.22), and from
/// `gtk-enable-animations` before that.
const REDUCED_MOTION: &str = "gtk-interface-reduced-motion";

thread_local! {
    /// The system's values, put back when Reduce Motion is turned off.
    static SYSTEM_MOTION: OnceCell<(bool, Option<glib::Value>)> = const { OnceCell::new() };
}

fn reduced_motion_value(nick: &str) -> Option<glib::Value> {
    glib::EnumClass::with_type(glib::Type::from_name("GtkReducedMotion")?)?.to_value_by_nick(nick)
}

pub fn apply_motion(reduce: bool) {
    let Some(gtk_settings) = gtk::Settings::default() else {
        return;
    };
    let has_reduced = gtk_settings.find_property(REDUCED_MOTION).is_some();
    let (animate, reduced) = SYSTEM_MOTION.with(|system| {
        system
            .get_or_init(|| {
                (
                    gtk_settings.is_gtk_enable_animations(),
                    has_reduced.then(|| gtk_settings.property_value(REDUCED_MOTION)),
                )
            })
            .clone()
    });
    let (animate, reduced) = if reduce {
        (
            false,
            reduced_motion_value("reduce").filter(|_| has_reduced),
        )
    } else {
        (animate, reduced)
    };
    gtk_settings.set_gtk_enable_animations(animate);
    if let Some(value) = reduced {
        gtk_settings.set_property_from_value(REDUCED_MOTION, &value);
    }
    // WebKit re-reads the system settings when the theme name changes.
    gtk_settings.notify("gtk-theme-name");
}
